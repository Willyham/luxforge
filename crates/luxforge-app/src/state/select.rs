//! The Select workspace's view model ([catalog design](../../../../docs/design/catalog.md#workspaces)):
//! which workspace the window shows, what the Select workspace last read from the owner — the event
//! list, the view's summary and facets, and a bounded window of its rows — and the plain data its
//! regions are drawn from: the title bar, the sources panel, the filter bar and the floating strip,
//! the grid's blocks and cells, the Info panel and the status line. **Lane D (views and desktop)**
//! owns it. Like every view model it names no framework type, no widget and no view.
//!
//! The desktop holds no catalog logic. A change of source, filter, sort or grouping is a whole
//! [`ViewQuery`] for the owner to evaluate ([`changed`]); a selection gesture is the `browse.select`
//! request an API client would send ([`select_params`]); a pick, Pick all, and library undo and
//! redo are the `pick.set`, `library.undo` and `library.redo` requests an agent sends
//! ([`pick_params`], [`library_params`]); the grid's blocks are the summary's group layout as the
//! owner answered it, with its days' and moments' pick counts ([`grid_content`]).
use super::{Inputs, status::clients_text};
use luxforge_core::{
    MutationRequest, SourceTag,
    catalog_types::{
        BodyKey, BracketEvidence, BrowseSession, Cards, CatalogCounts, DiskFolders, Event,
        EventList, Exposure, Facet, Facets, FileAvailability, FileId, Grouping, IndexSource,
        LibraryChange, LibraryItem, LocalDay, Moment, MomentKind, Month, PreviewState, RowItem,
        SortKey, Targets, ViewQuery, ViewRow, ViewSource, ViewSummary, VolumeId, Volumes,
    },
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    path::{Path, PathBuf},
};

#[cfg(test)]
mod tests;

/// Rows one `browse.rows` request reads. The view is read in blocks of this many, aligned to it, so
/// scrolling back over a block already read costs nothing.
pub(crate) const ROW_BLOCK: u32 = 200;
/// The most blocks the desktop keeps, 3,200 rows: those nearest the screen. However large the view,
/// this is all of it the desktop holds.
pub(crate) const ROW_BLOCKS_KEPT: usize = 16;
/// The size slider's range and step, in points of cell width.
pub(crate) const CELL_WIDTH_MIN: f32 = 96.0;
pub(crate) const CELL_WIDTH_MAX: f32 = 320.0;
pub(crate) const CELL_WIDTH_STEP: f32 = 4.0;
/// The two cell presets' own widths: the event board's 136 pt Select cell and the catalog board's
/// 168 pt cell.
pub(crate) const FILES_CELL_WIDTH: f32 = 136.0;
pub(crate) const CATALOG_CELL_WIDTH: f32 = 168.0;

/// Which workspace the window shows. This is the desktop's own view state, like the developer
/// gallery page: the owner's session does not carry it, and switching commits, discards or pauses
/// nothing. Evidence records it with every captured frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Shown {
    #[default]
    Develop,
    Select,
}

impl Shown {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Develop => "develop",
            Self::Select => "select",
        }
    }
}

/// A chip's or the sort's menu, open under it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectMenu {
    Camera,
    Kind,
    Group,
    Sort,
}

/// One of Select's two side panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectPanel {
    Sources,
    Info,
}

/// The filter bar's pick segments over files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PickFilter {
    All,
    Picked,
    WithoutPick,
}

impl PickFilter {
    pub(crate) const ALL: [Self; 3] = [Self::All, Self::Picked, Self::WithoutPick];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Picked => "Picked",
            Self::WithoutPick => "Moments without a pick",
        }
    }
}

/// One change the filter bar, the Group chip or the sort menu makes to the view's query. Each
/// changes its own part of the query and nothing else ([`changed`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum QueryChange {
    Pick(PickFilter),
    /// One camera body, or every camera.
    Camera(Option<BodyKey>),
    /// One kind, or both.
    Kind(Option<SourceTag>),
    Group(Grouping),
    Sort(SortKey),
}

/// What the Select workspace holds that its model reads: which workspace is shown, its panels, what
/// it last read from the owner and the local choices it sends. The app keeps it in its Select seam
/// with the grid's layout, scroll and viewport, which are the widget's.
#[derive(Clone, Debug)]
pub(crate) struct SelectState {
    pub(crate) shown: Shown,
    pub(crate) sources_panel: bool,
    pub(crate) info_panel: bool,
    /// The sources panel's search text, which is `event.list`'s `query`.
    pub(crate) search: String,
    /// The events as `event.list` last answered them for the search text.
    pub(crate) events: Option<EventList>,
    pub(crate) events_error: Option<String>,
    /// The folder browsed On disk, with its subfolders.
    pub(crate) folder: Option<PathBuf>,
    /// The card or folder being read for this desktop while it waits to replace the view (not
    /// sent to the background): the title names it.
    pub(crate) reading: Option<ReadSource>,
    /// The query the desktop last sent, which the sources panel and the filter bar show while its
    /// answer is on its way.
    pub(crate) query: Option<ViewQuery>,
    /// A `browse.view` is in flight.
    pub(crate) loading: bool,
    /// The view as the owner last evaluated it.
    pub(crate) summary: Option<ViewSummary>,
    /// Why the last evaluation failed, until one succeeds.
    pub(crate) view_error: Option<String>,
    /// The Camera, Kind and Pick counts of the view's source and filter.
    pub(crate) facets: Option<Facets>,
    /// The rows read near the screen.
    pub(crate) rows: RowCache,
    /// The grid's blocks and footer labels, made from the summary when it arrives or a burst is
    /// collapsed or expanded.
    pub(crate) content: GridContent,
    /// The bursts collapsed to one cell, by their index in the summary's moments: the desktop's own
    /// view state, kept while the same query is evaluated again and cleared with a new one.
    pub(crate) collapsed: BTreeSet<u32>,
    pub(crate) menu: Option<SelectMenu>,
    /// The loupe over the centre ([`super::loupe`]).
    pub(crate) loupe: super::loupe::LoupeState,
    /// Missing originals, drawn in place of the grid while it is the source
    /// ([`super::select_missing`]).
    pub(crate) missing: super::select_missing::MissingState,
    /// The catalog's folders and collections, its filter bar and Metadata browser, and the Info
    /// panel over photographs ([`super::select_catalog`]).
    pub(crate) catalog: super::select_catalog::CatalogState,
    /// The size slider's cell widths, one per preset.
    pub(crate) files_cell_width: f32,
    pub(crate) catalog_cell_width: f32,
    /// The home folder, which paths are shown under as `~`.
    pub(crate) home: Option<PathBuf>,
    /// The mounted camera cards, as `card.list` last answered, read each time Select is shown.
    pub(crate) cards: Option<Cards>,
    /// The volumes On disk lists, as `volume.list` last answered, read with the cards.
    pub(crate) volumes: Option<Volumes>,
    /// The subfolders `disk.folders` answered for each volume or folder opened On disk.
    pub(crate) disk: BTreeMap<PathBuf, DiskFolders>,
    /// The volumes (by mount point) and folders open On disk: the desktop's own view state.
    pub(crate) open: BTreeSet<PathBuf>,
    /// The catalog's counts behind the Catalog sources, as `catalog.info` last answered, read when
    /// Select is shown and after a library change.
    pub(crate) counts: Option<CatalogCounts>,
}

impl Default for SelectState {
    fn default() -> Self {
        Self {
            shown: Shown::Develop,
            sources_panel: true,
            info_panel: true,
            search: String::new(),
            events: None,
            events_error: None,
            folder: None,
            reading: None,
            query: None,
            loading: false,
            summary: None,
            view_error: None,
            facets: None,
            rows: RowCache::default(),
            content: GridContent::default(),
            collapsed: BTreeSet::new(),
            menu: None,
            loupe: super::loupe::LoupeState::default(),
            missing: super::select_missing::MissingState::default(),
            catalog: super::select_catalog::CatalogState::default(),
            files_cell_width: FILES_CELL_WIDTH,
            catalog_cell_width: CATALOG_CELL_WIDTH,
            home: None,
            cards: None,
            volumes: None,
            disk: BTreeMap::new(),
            open: BTreeSet::new(),
            counts: None,
        }
    }
}

impl SelectState {
    /// The view lists developed photographs, which the catalog's larger cells draw.
    pub(crate) fn over_catalog(&self) -> bool {
        self.query
            .as_ref()
            .is_some_and(|query| !query.source.over_files())
    }

    /// The size slider's cell width for the preset in use.
    pub(crate) fn cell_width(&self) -> f32 {
        if self.over_catalog() {
            self.catalog_cell_width
        } else {
            self.files_cell_width
        }
    }

    /// Set the size slider's cell width for the preset in use, clamped to its range and step.
    pub(crate) fn set_cell_width(&mut self, width: f32) {
        let width = clamp_cell_width(width);
        if self.over_catalog() {
            self.catalog_cell_width = width;
        } else {
            self.files_cell_width = width;
        }
    }

    /// The view revision the held summary names.
    pub(crate) fn revision(&self) -> Option<u64> {
        self.summary.as_ref().map(|summary| summary.revision)
    }

    /// The burst `item` is a frame of: its index in the summary's moments, when it is one.
    pub(crate) fn burst_of(&self, item: u32) -> Option<u32> {
        let moments = &self.summary.as_ref()?.groups.moments;
        let after = moments.partition_point(|moment| moment.start <= item);
        let index = after.checked_sub(1)?;
        let moment = &moments[index];
        (moment.kind == MomentKind::Burst && item < moment.start.saturating_add(moment.len))
            .then_some(index as u32)
    }
}

/// A cell width clamped to the slider's range and snapped to its step.
pub(crate) fn clamp_cell_width(width: f32) -> f32 {
    if !width.is_finite() {
        return FILES_CELL_WIDTH;
    }
    ((width / CELL_WIDTH_STEP).round() * CELL_WIDTH_STEP).clamp(CELL_WIDTH_MIN, CELL_WIDTH_MAX)
}

// -- Queries ---------------------------------------------------------------------------------------

/// The query a source is first viewed with: every part at its default, except that the catalog's
/// photographs are shown newest first and ungrouped, as the catalog board draws them.
pub(crate) fn source_query(source: ViewSource) -> ViewQuery {
    let mut query = ViewQuery::of(source);
    if !query.source.over_files() {
        query.sort.descending = true;
        query.grouping = Grouping::None;
    }
    query
}

/// `query` with one change made, and nothing else changed.
pub(crate) fn changed(query: &ViewQuery, change: &QueryChange) -> ViewQuery {
    let mut query = query.clone();
    match change {
        QueryChange::Pick(pick) => {
            let (picked, without_pick) = match pick {
                PickFilter::All => (None, false),
                PickFilter::Picked => (Some(true), false),
                PickFilter::WithoutPick => (None, true),
            };
            query.filter.picked = picked;
            query.filter.without_pick = without_pick;
        }
        QueryChange::Camera(camera) => query.filter.cameras = camera.iter().cloned().collect(),
        QueryChange::Kind(kind) => query.filter.kinds = kind.iter().copied().collect(),
        QueryChange::Group(grouping) => query.grouping = *grouping,
        QueryChange::Sort(key) => {
            query.sort.key = *key;
            // Newest first over the catalog, except by name; a shoot is browsed in order.
            query.sort.descending = !query.source.over_files() && *key != SortKey::FileName;
        }
    }
    query
}

/// Which pick segment a query's filter is.
pub(crate) fn pick_filter(query: &ViewQuery) -> PickFilter {
    if query.filter.without_pick {
        PickFilter::WithoutPick
    } else if query.filter.picked == Some(true) {
        PickFilter::Picked
    } else {
        PickFilter::All
    }
}

/// The sorts the strip offers: capture time and file name over files, and the catalog's own two as
/// well over photographs.
pub(crate) fn sorts(over_files: bool) -> &'static [SortKey] {
    if over_files {
        &[SortKey::CaptureTime, SortKey::FileName]
    } else {
        &[
            SortKey::CaptureTime,
            SortKey::DateDeveloped,
            SortKey::FileName,
            SortKey::LastEdited,
        ]
    }
}

pub(crate) fn sort_label(key: SortKey) -> &'static str {
    match key {
        SortKey::CaptureTime => "Capture time",
        SortKey::DateDeveloped => "Date developed",
        SortKey::FileName => "File name",
        SortKey::LastEdited => "Last edited",
    }
}

pub(crate) fn grouping_label(grouping: Grouping) -> &'static str {
    match grouping {
        Grouping::DayCameraMoment => "Day \u{203a} Camera \u{203a} Moment",
        Grouping::Day => "Day",
        Grouping::None => "None",
    }
}

/// `browse.view`'s parameters: the whole query as fields, and nothing else.
pub(crate) fn view_params(query: &ViewQuery) -> Value {
    serde_json::to_value(query).unwrap_or_default()
}

/// `browse.facets`' parameters for the chips' menus: the query's source and filter, and the Camera,
/// Kind and (over files) Pick counts.
pub(crate) fn facets_params(query: &ViewQuery) -> Value {
    // Over the catalog, the Metadata browser's columns too.
    if !query.source.over_files() {
        return super::select_catalog::facets_params(query);
    }
    let mut facets = vec![Facet::Camera, Facet::Kind];
    if query.source.over_files() {
        facets.push(Facet::Pick);
    }
    json!({"source": query.source, "filter": query.filter, "facets": facets})
}

/// `event.list`'s parameters for the sources panel's search text.
pub(crate) fn events_params(search: &str) -> Value {
    let query = search.trim();
    if query.is_empty() {
        json!({})
    } else {
        json!({ "query": query })
    }
}

/// `browse.rows`' parameters for one window of the view at `revision`.
pub(crate) fn rows_params(request: &RowsRequest) -> Value {
    json!({"from": request.from, "count": request.count, "revision": request.revision})
}

// -- Selection -------------------------------------------------------------------------------------

/// A selection gesture, in view positions. A cell is its first item and its span: a collapsed
/// burst's cell covers all its frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectGesture {
    /// A click or an arrow key: the selection becomes this cell's items, and its item is active.
    Only { item: u32, span: u32 },
    /// Cmd-click: this cell's items join or leave the selection. Joining makes its item active.
    Toggle { item: u32, span: u32, adding: bool },
    /// Shift-click or Shift and an arrow: the selection becomes everything from the anchor's cell
    /// to the target's, and the target is active.
    Extend {
        anchor: (u32, u32),
        target: (u32, u32),
    },
    /// Cmd+A: every item in the view.
    All,
    /// Cmd+D: nothing.
    Nothing,
}

/// The `browse.select` request a gesture sends, exactly as an API client would write it, against
/// the view revision the desktop holds.
pub(crate) fn select_params(gesture: SelectGesture, revision: Option<u64>) -> Value {
    let range = |start: u32, len: u32| json!({"start": start, "len": len.max(1)});
    let mut params = match gesture {
        SelectGesture::Only { item, span } => {
            json!({"mode": "replace", "range": range(item, span), "active": item})
        }
        SelectGesture::Toggle { item, span, adding } => {
            let mut params = json!({"mode": "toggle", "range": range(item, span)});
            if adding {
                params["active"] = json!(item);
            }
            params
        }
        SelectGesture::Extend { anchor, target } => {
            let start = anchor.0.min(target.0);
            let end = (anchor.0 + anchor.1.max(1)).max(target.0 + target.1.max(1));
            json!({"mode": "replace", "range": range(start, end - start), "active": target.0})
        }
        SelectGesture::All => json!({"mode": "replace", "all": true}),
        SelectGesture::Nothing => json!({"mode": "remove", "all": true}),
    };
    if let Some(revision) = revision {
        params["revision"] = json!(revision);
    }
    params
}

// -- Picking and library changes -------------------------------------------------------------------

/// A library change a Select gesture makes, which says how its answer is told in the status bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LibraryGesture {
    /// `P` or the Info panel's Pick: pick (`true`) or clear the selection, or the loupe's frame.
    Pick { picked: bool },
    /// A bracket's Pick all.
    PickAll,
    /// `Cmd+Z` or the title bar's Undo.
    Undo,
    /// `Shift+Cmd+Z` or the title bar's Redo.
    Redo,
    /// Organizing the catalog: a folder, a collection or the selected photographs.
    Catalog(super::select_catalog::CatalogGesture),
}

impl LibraryGesture {
    /// The method the gesture sends.
    pub(crate) fn method(self) -> &'static str {
        match self {
            Self::Pick { .. } | Self::PickAll => "pick.set",
            Self::Undo => "library.undo",
            Self::Redo => "library.redo",
            Self::Catalog(gesture) => gesture.method(),
        }
    }

    /// What the status bar says when the owner changed nothing.
    pub(crate) fn nothing(self) -> &'static str {
        match self {
            Self::Catalog(gesture) => gesture.nothing(),
            Self::Pick { picked: true } | Self::PickAll => "Already picked",
            Self::Pick { picked: false } => "Nothing to clear",
            Self::Undo => "Nothing to undo",
            Self::Redo => "Nothing to redo",
        }
    }

    /// What the status bar says of a recorded change whose label could not be read.
    pub(crate) fn done(self) -> &'static str {
        match self {
            Self::Catalog(gesture) => gesture.done(),
            Self::Pick { picked: true } => "Picked \u{b7} Undo \u{2318}Z",
            Self::Pick { picked: false } => "Cleared the picks \u{b7} Undo \u{2318}Z",
            Self::PickAll => "Picked the frames \u{b7} Undo \u{2318}Z",
            Self::Undo => "Undid the last library change \u{b7} Redo \u{21e7}\u{2318}Z",
            Self::Redo => "Redid the last library change \u{b7} Undo \u{2318}Z",
        }
    }

    /// What a refusal is prefixed with.
    pub(crate) fn refused(self) -> &'static str {
        match self {
            Self::Catalog(gesture) => gesture.refused(),
            Self::Pick { picked: true } | Self::PickAll => "Could not pick",
            Self::Pick { picked: false } => "Could not clear",
            Self::Undo => "Could not undo",
            Self::Redo => "Could not redo",
        }
    }
}

/// `pick.set`'s parameters: pick or clear `targets` as `mutation`, exactly as an agent writes them.
pub(crate) fn pick_params(targets: &Targets, picked: bool, mutation: &MutationRequest) -> Value {
    json!({"targets": targets, "picked": picked, "mutation": mutation})
}

/// `library.undo`'s and `library.redo`'s parameters: the envelope alone. Its actor is the undo's
/// scope, so this desktop undoes only its own changes.
pub(crate) fn library_params(mutation: &MutationRequest) -> Value {
    json!({ "mutation": mutation })
}

/// `library.journal`'s parameters for the one change `sequence`: the page after the change before
/// it, one long. Its label is what the status bar says.
pub(crate) fn journal_params(sequence: u64) -> Value {
    json!({"after": sequence.saturating_sub(1), "limit": 1})
}

/// Whether `P` picks (`true`) or clears (`false`) the selection. The desktop knows a selected
/// item is picked only from a row it has read, so it clears only when it holds the row of every
/// selected item and each is picked; otherwise it picks, which leaves an item already picked as it
/// was. So `P` never clears a pick the desktop has not shown.
pub(crate) fn pick_value(selection: &SelectionModel, rows: &RowCache) -> bool {
    if selection.count == 0 || selection.count as usize > rows.len() {
        return true;
    }
    !selection
        .ranges
        .iter()
        .flat_map(|&(start, end)| start..end)
        .all(|position| rows.read(position).is_some_and(|row| row.picked))
}

/// The status bar's sentence for a recorded library change: its label from the journal, as a
/// person reads it, with the key that takes it back. An undo says what it undid and offers redo.
pub(crate) fn change_text(change: &LibraryChange) -> String {
    if change.undoes.is_some() {
        let undone = change.label.strip_prefix("Undo ").unwrap_or(&change.label);
        format!("Undid {undone} \u{b7} Redo \u{21e7}\u{2318}Z")
    } else if change.redoes.is_some() {
        let redone = change.label.strip_prefix("Redo ").unwrap_or(&change.label);
        format!("Redid {redone} \u{b7} Undo \u{2318}Z")
    } else {
        format!("{} \u{b7} Undo \u{2318}Z", change.label)
    }
}

/// What a refused undo or redo says: the first item that changed since, by its file's name where
/// it has one, and how many others did, from the refusal's `data` (`{items, count}`). `None` when
/// the refusal carries no items.
pub(crate) fn refusal_text(gesture: LibraryGesture, data: Option<&Value>) -> Option<String> {
    let data = data?;
    let first: LibraryItem = serde_json::from_value(data.get("items")?.get(0)?.clone()).ok()?;
    let count = data.get("count").and_then(Value::as_u64).unwrap_or(1);
    let name = match &first {
        LibraryItem::Pick { path } | LibraryItem::IndexedFolder { path } => {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        }
        LibraryItem::CatalogFolder { .. } => "a catalog folder".to_owned(),
        LibraryItem::Collection { .. } => "a collection".to_owned(),
        _ => "a photograph".to_owned(),
    };
    let others = match count.saturating_sub(1) {
        0 => String::new(),
        1 => " and 1 other item".to_owned(),
        more => format!(" and {more} other items"),
    };
    Some(format!(
        "{}: {name}{others} changed since",
        gesture.refused()
    ))
}

/// The files of the view's frames `start..start + len` whose rows are read, in order; `None` when
/// any is not read yet or is a photograph. What Pick all names.
pub(crate) fn frame_files(rows: &RowCache, start: u32, len: u32) -> Option<Vec<FileId>> {
    (start..start.saturating_add(len))
        .map(|position| match rows.row(position)?.item {
            RowItem::File { file_id } => Some(file_id),
            RowItem::Photo { .. } => None,
        })
        .collect()
}

/// The session's selection in the view on screen, as the grid draws it: disjoint ascending ranges
/// of positions and the active item. Empty when the session describes another revision of the view
/// than the one drawn.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SelectionModel {
    /// `start..end` ranges.
    pub(crate) ranges: Vec<(u32, u32)>,
    pub(crate) count: u32,
    pub(crate) active: Option<u32>,
}

impl SelectionModel {
    pub(crate) fn of(browse: &BrowseSession, revision: Option<u64>) -> Self {
        if revision != Some(browse.revision) {
            return Self::default();
        }
        let selection = &browse.selection;
        Self {
            ranges: selection
                .ranges
                .iter()
                .filter(|range| range.len > 0)
                .map(|range| (range.start, range.start.saturating_add(range.len)))
                .collect(),
            count: selection.count,
            active: selection.active,
        }
    }

    /// Whether any of a cell's items is selected.
    pub(crate) fn selected(&self, item: u32, span: u32) -> bool {
        let end = item.saturating_add(span.max(1));
        let before = self.ranges.partition_point(|&(start, _)| start < end);
        before > 0 && self.ranges[before - 1].1 > item
    }

    /// Whether the active item is one of a cell's items.
    pub(crate) fn active_in(&self, item: u32, span: u32) -> bool {
        self.active
            .is_some_and(|active| active >= item && active < item.saturating_add(span.max(1)))
    }

    /// The item the Info panel describes: the active one, or the first selected.
    pub(crate) fn focus(&self) -> Option<u32> {
        self.active
            .or_else(|| self.ranges.first().map(|&(start, _)| start))
    }
}

// -- The grid's blocks -----------------------------------------------------------------------------

/// One piece of the grid's structure, in view order: what the grid widget lays out, as plain data.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Block {
    Day {
        title: String,
        detail: String,
    },
    Camera {
        title: String,
        detail: String,
    },
    /// A burst or a bracket of `frames` consecutive items, with its header: its pick count in the
    /// accent ("1 picked") and, for a bracket over files not all picked, its action ("Pick all
    /// 3"). `index` is the moment's in the summary's `groups.moments`.
    Moment {
        index: u32,
        bracket: bool,
        title: String,
        detail: String,
        evidence: Option<String>,
        picked: Option<String>,
        action: Option<String>,
        frames: u32,
        collapsed: bool,
    },
    Singles(u32),
}

/// The grid's blocks and each bracket frame's footer label, by view position.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GridContent {
    pub(crate) blocks: Vec<Block>,
    /// Ascending by position.
    pub(crate) labels: Vec<(u32, String)>,
}

impl GridContent {
    /// The summary's index of the grid's moment `number`: the moments counted in order, collapsed
    /// ones included, as the grid names a header's action.
    pub(crate) fn moment(&self, number: u32) -> Option<u32> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                Block::Moment { index, .. } => Some(*index),
                _ => None,
            })
            .nth(number as usize)
    }

    /// The footer label of the cell whose first item is `item`.
    pub(crate) fn label(&self, item: u32) -> Option<&str> {
        let index = self
            .labels
            .binary_search_by_key(&item, |(position, _)| *position)
            .ok()?;
        Some(self.labels[index].1.as_str())
    }

    /// How many items the blocks cover.
    #[cfg(test)]
    pub(crate) fn items(&self) -> u32 {
        self.blocks
            .iter()
            .map(|block| match block {
                Block::Moment { frames, .. } => *frames,
                Block::Singles(count) => *count,
                _ => 0,
            })
            .sum()
    }
}

/// The grid's blocks for a summary: a heading where each day and each camera group starts, a moment
/// block for each burst and bracket, and the frames between them as singles. It follows the group
/// layout the owner answered, whatever the grouping: a view with no days has no headings, and one
/// with no moments is all singles. A day says how many of its frames are picked, a moment how many
/// of its frames are ("1 picked"), and a bracket of files not all picked offers Pick all.
/// `collapsed` names the bursts drawn as one cell.
pub(crate) fn grid_content(summary: &ViewSummary, collapsed: &BTreeSet<u32>) -> GridContent {
    let count = summary.count;
    let over_files = summary.query.source.over_files();
    let groups = &summary.groups;
    let mut content = GridContent::default();
    let (mut day, mut camera, mut moment) = (0, 0, 0);
    let mut position = 0;
    while position < count {
        // Headings start where their groups do; anything the layout names behind the position is
        // malformed and skipped rather than drawn out of order.
        while groups
            .days
            .get(day)
            .is_some_and(|group| group.start < position)
        {
            day += 1;
        }
        while groups
            .cameras
            .get(camera)
            .is_some_and(|group| group.start < position)
        {
            camera += 1;
        }
        while groups
            .moments
            .get(moment)
            .is_some_and(|group| group.start < position)
        {
            moment += 1;
        }
        if let Some(group) = groups.days.get(day).filter(|group| group.start == position) {
            let mut detail = photographs(group.len.min(count - position));
            if group.picked > 0 {
                detail = format!("{detail} \u{b7} {} picked", thousands(group.picked));
            }
            content.blocks.push(Block::Day {
                title: day_title(group.day),
                detail,
            });
            day += 1;
        }
        if let Some(group) = groups
            .cameras
            .get(camera)
            .filter(|group| group.start == position)
        {
            content.blocks.push(Block::Camera {
                title: group.label.clone(),
                detail: thousands(group.len.min(count - position)),
            });
            camera += 1;
        }
        if let Some(group) = groups
            .moments
            .get(moment)
            .filter(|group| group.start == position && group.kind != MomentKind::Single)
        {
            let frames = group.len.min(count - position);
            if frames > 0 {
                let bracket = group.kind == MomentKind::Bracket;
                content.blocks.push(Block::Moment {
                    index: moment as u32,
                    bracket,
                    title: if bracket { "Bracket" } else { "Burst" }.to_owned(),
                    detail: moment_detail(group),
                    evidence: group.evidence.map(evidence_label).map(str::to_owned),
                    picked: (group.picked > 0)
                        .then(|| format!("{} picked", thousands(group.picked))),
                    action: (bracket && over_files && group.picked < frames)
                        .then(|| format!("Pick all {frames}")),
                    frames,
                    collapsed: !bracket && collapsed.contains(&(moment as u32)),
                });
                if bracket && group.steps_ev.len() == group.len as usize {
                    for (frame, step) in group.steps_ev.iter().take(frames as usize).enumerate() {
                        content
                            .labels
                            .push((position + frame as u32, format!("{} EV", ev_step(*step))));
                    }
                }
                position += frames;
                moment += 1;
                continue;
            }
            moment += 1;
        }
        // Singles, up to where the next group starts.
        let next = [
            groups.days.get(day).map(|group| group.start),
            groups.cameras.get(camera).map(|group| group.start),
            groups.moments.get(moment).map(|group| group.start),
        ]
        .into_iter()
        .flatten()
        .filter(|&start| start > position)
        .min()
        .unwrap_or(count)
        .min(count);
        let run = next - position;
        match content.blocks.last_mut() {
            Some(Block::Singles(singles)) => *singles += run,
            _ => content.blocks.push(Block::Singles(run)),
        }
        position = next;
    }
    content
}

/// A moment's header detail: a burst's frames and span, a bracket's exposures and steps.
fn moment_detail(moment: &Moment) -> String {
    match moment.kind {
        MomentKind::Bracket => {
            let exposures = format!("{} exposures", moment.len);
            if moment.steps_ev.is_empty() {
                exposures
            } else {
                format!("{exposures} \u{b7} {} EV", steps_text(&moment.steps_ev))
            }
        }
        _ => format!("{} frames in {}", moment.len, span_text(moment.span_ms)),
    }
}

fn evidence_label(evidence: BracketEvidence) -> &'static str {
    match evidence {
        BracketEvidence::Metadata => "from metadata",
        BracketEvidence::Previews => "from previews",
    }
}

/// A bracket's steps: `−2 · 0 · +2`.
fn steps_text(steps: &[f32]) -> String {
    steps
        .iter()
        .map(|step| ev_step(*step))
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// One exposure step to a tenth of a stop, signed, with the minus sign: `−0.7`, `0`, `+2`.
pub(crate) fn ev_step(step: f32) -> String {
    let tenths = (step * 10.0).round() / 10.0;
    if tenths == 0.0 || !tenths.is_finite() {
        return "0".to_owned();
    }
    let magnitude = if tenths.fract() == 0.0 {
        format!("{}", tenths.abs() as i64)
    } else {
        format!("{:.1}", tenths.abs())
    };
    format!("{}{magnitude}", if tenths < 0.0 { '\u{2212}' } else { '+' })
}

/// A moment's span: to a tenth of a second under 10 s, whole seconds under a minute, then minutes
/// and seconds.
pub(crate) fn span_text(ms: u64) -> String {
    if ms < 10_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else if ms < 60_000 {
        format!("{} s", (ms as f64 / 1000.0).round() as u64)
    } else {
        let seconds = (ms as f64 / 1000.0).round() as u64;
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}

// -- Rows ------------------------------------------------------------------------------------------

/// One `browse.rows` request: a block of the view at a revision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowsRequest {
    pub(crate) revision: u64,
    pub(crate) from: u32,
    pub(crate) count: u32,
}

/// The blocks of rows that cover `items` of a `count`-item view, at most [`ROW_BLOCKS_KEPT`] of them
/// from the first.
pub(crate) fn wanted_blocks(items: Range<u32>, count: u32) -> Range<u32> {
    let end = items.end.min(count);
    if items.start >= end {
        return 0..0;
    }
    let first = items.start / ROW_BLOCK;
    let last = (end - 1) / ROW_BLOCK + 1;
    first..last.min(first + ROW_BLOCKS_KEPT as u32)
}

/// The rows the desktop has read of the view on screen: whole blocks of [`ROW_BLOCK`], at most
/// [`ROW_BLOCKS_KEPT`] of them, one request in flight. A view evaluated again starts it over, and a
/// block the owner refused is not asked for again until then.
///
/// A view of the same source evaluated again may carry a few rows of its previous revision — the
/// active item's and its neighbours' ([`carried_rows`]) — each at its position in the new one, so
/// the active frame keeps its row until the new revision's block replaces it. A carried row is
/// never counted as read: its block is still wanted.
#[derive(Clone, Debug, Default)]
pub(crate) struct RowCache {
    revision: u64,
    count: u32,
    blocks: BTreeMap<u32, Vec<ViewRow>>,
    carried: BTreeMap<u32, ViewRow>,
    in_flight: Option<RowsRequest>,
    failed: BTreeSet<u32>,
}

/// How many rows either side of the active item a view evaluated again carries over, when its
/// items keep their positions: the loupe's moment, its strip and its look-ahead.
pub(crate) const CARRIED_NEIGHBOURS: u32 = 32;

impl RowCache {
    /// Forget every row: the view is now `count` items at `revision`.
    pub(crate) fn reset(&mut self, revision: u64, count: u32) {
        *self = Self {
            revision,
            count,
            ..Self::default()
        };
    }

    /// Forget every row but `carried`, each already at its position at `revision`, until the new
    /// revision's own rows replace them.
    pub(crate) fn reset_carrying(&mut self, revision: u64, count: u32, carried: Vec<ViewRow>) {
        self.reset(revision, count);
        self.carried = carried
            .into_iter()
            .filter(|row| row.position < count)
            .map(|row| (row.position, row))
            .collect();
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    /// How many rows are held.
    pub(crate) fn len(&self) -> usize {
        self.blocks.values().map(Vec::len).sum()
    }

    /// How many blocks are held.
    pub(crate) fn blocks(&self) -> usize {
        self.blocks.len()
    }

    /// The request in flight, if one is.
    pub(crate) fn in_flight(&self) -> Option<RowsRequest> {
        self.in_flight
    }

    /// Whether a block of `wanted` items is still to be read: neither held nor refused.
    pub(crate) fn wants(&self, wanted: Range<u32>) -> bool {
        wanted_blocks(wanted, self.count)
            .any(|block| !self.blocks.contains_key(&block) && !self.failed.contains(&block))
    }

    /// The row read at `position` in this revision, never one carried from the last: what a pick
    /// or clear decides by, since a carried row's pick may be the one before the change.
    pub(crate) fn read(&self, position: u32) -> Option<&ViewRow> {
        self.blocks
            .get(&(position / ROW_BLOCK))?
            .get((position % ROW_BLOCK) as usize)
    }

    /// The row to draw at `position`: this revision's, or else one carried from the last.
    pub(crate) fn row(&self, position: u32) -> Option<&ViewRow> {
        match self.blocks.get(&(position / ROW_BLOCK)) {
            Some(block) => block.get((position % ROW_BLOCK) as usize),
            None => self.carried.get(&position),
        }
    }

    /// What the grid draws for the cell whose first item is `position`, once its row is read.
    pub(crate) fn cell(&self, position: u32) -> Option<CellFacts> {
        self.row(position).map(cell_facts)
    }

    /// The item a cell of `span` items from `item` shows: a collapsed burst shows its pick, the
    /// first of its frames read as picked, or else its first frame; any other cell its one item.
    pub(crate) fn shown(&self, item: u32, span: u32) -> u32 {
        if span <= 1 {
            return item;
        }
        (item..item.saturating_add(span))
            .find(|&position| self.row(position).is_some_and(|row| row.picked))
            .unwrap_or(item)
    }

    /// The next block to read for `wanted` items, counted in flight until it is answered; `None`
    /// while one is in flight or every wanted block is held or refused.
    pub(crate) fn next_request(&mut self, wanted: Range<u32>) -> Option<RowsRequest> {
        if self.in_flight.is_some() {
            return None;
        }
        let block = wanted_blocks(wanted, self.count)
            .find(|block| !self.blocks.contains_key(block) && !self.failed.contains(block))?;
        let from = block * ROW_BLOCK;
        let request = RowsRequest {
            revision: self.revision,
            from,
            count: ROW_BLOCK.min(self.count - from),
        };
        self.in_flight = Some(request);
        Some(request)
    }

    /// Keep the rows a request answered, if they belong to this revision, then drop the blocks
    /// furthest from `wanted` past the bound. Answers whether they were kept.
    pub(crate) fn answered(
        &mut self,
        revision: u64,
        from: u32,
        rows: Vec<ViewRow>,
        wanted: Range<u32>,
    ) -> bool {
        if revision != self.revision || !from.is_multiple_of(ROW_BLOCK) {
            return false;
        }
        let block = from / ROW_BLOCK;
        if self.in_flight.is_some_and(|request| request.from == from) {
            self.in_flight = None;
        }
        // The block's own rows replace what was carried into it.
        self.carried
            .retain(|position, _| !(from..from + ROW_BLOCK).contains(position));
        self.blocks.insert(block, rows);
        let keep = wanted_blocks(wanted, self.count);
        while self.blocks.len() > ROW_BLOCKS_KEPT {
            let distance = |block: u32| {
                if block < keep.start {
                    keep.start - block
                } else {
                    block.saturating_sub(keep.end.saturating_sub(1))
                }
            };
            let Some(furthest) = self
                .blocks
                .keys()
                .copied()
                .max_by_key(|&block| distance(block))
            else {
                break;
            };
            self.blocks.remove(&furthest);
        }
        true
    }

    /// A request of this revision failed: its block is not asked for again until the view is
    /// evaluated again.
    pub(crate) fn failed(&mut self, revision: u64, from: u32) {
        if revision != self.revision {
            return;
        }
        let block = from / ROW_BLOCK;
        if self.in_flight.is_some_and(|request| request.from == from) {
            self.in_flight = None;
        }
        self.failed.insert(block);
    }
}

/// The rows of the previous revision a view of the same source evaluated again carries, each at its
/// new position: the active item's row always, since the owner carries the active item over by
/// item and `now_active` is where it put it; and, when the view kept its count and the active item
/// its position, so its items kept theirs, the rows within [`CARRIED_NEIGHBOURS`] of it.
pub(crate) fn carried_rows(
    rows: &RowCache,
    was_active: Option<u32>,
    now_active: Option<u32>,
    now_count: u32,
) -> Vec<ViewRow> {
    let (Some(was), Some(now)) = (was_active, now_active) else {
        return Vec::new();
    };
    let Some(active) = rows.row(was) else {
        return Vec::new();
    };
    if was != now || rows.count != now_count {
        let mut row = active.clone();
        row.position = now;
        return vec![row];
    }
    let from = was.saturating_sub(CARRIED_NEIGHBOURS);
    let to = was
        .saturating_add(CARRIED_NEIGHBOURS)
        .min(now_count.saturating_sub(1));
    (from..=to)
        .filter_map(|position| rows.row(position).cloned())
        .collect()
}

/// A cell's file can be read: available, offline (its volume is not mounted) or unreadable (nothing
/// can be drawn for it).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Availability {
    #[default]
    Available,
    Offline,
    Unreadable,
}

/// What the grid draws for one cell from its row, as plain data.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct CellFacts {
    /// Width over height, upright, from the header's dimensions and orientation.
    pub(crate) aspect: Option<f32>,
    pub(crate) picked: bool,
    /// A file that is already a developed photograph's original.
    pub(crate) in_catalog: bool,
    pub(crate) availability: Availability,
    pub(crate) edited: bool,
}

pub(crate) fn cell_facts(row: &ViewRow) -> CellFacts {
    CellFacts {
        aspect: aspect(row),
        picked: row.picked,
        in_catalog: matches!(row.item, RowItem::File { .. }) && row.developed_as.is_some(),
        availability: if row.availability == FileAvailability::Offline {
            Availability::Offline
        } else if row.preview == PreviewState::Unavailable {
            Availability::Unreadable
        } else {
            Availability::Available
        },
        edited: row.edited,
    }
}

/// A row's upright width over height.
fn aspect(row: &ViewRow) -> Option<f32> {
    let dimensions = row.dimensions?;
    if dimensions.width == 0 || dimensions.height == 0 {
        return None;
    }
    let (width, height) = if row.orientation.is_some_and(|turn| turn.transposes()) {
        (dimensions.height, dimensions.width)
    } else {
        (dimensions.width, dimensions.height)
    };
    Some(width as f32 / height as f32)
}

// -- The model -------------------------------------------------------------------------------------

/// A source row's glyph.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceIcon {
    Event,
    Folder,
    /// A volume or a card.
    Drive,
    AllPhotographs,
    Recent,
    Missing,
    Removed,
}

/// A source row's trailing count.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Count {
    #[default]
    None,
    Total(String),
    Picks {
        picked: String,
        total: String,
    },
    /// Photographs whose originals are unavailable, in the clipping red: Missing originals'.
    Unavailable(String),
}

/// A source row's dot: a mounted volume's filled one, or the hollow one of an offline volume or
/// an event whose files are offline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dot {
    Mounted,
    Offline,
}

/// A source that is read before it is viewed: the index lane lists it and reads its headers
/// (`index.refresh`, a job), then it is viewed as the index listed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ReadSource {
    /// A folder on disk, with its subfolders.
    Folder(PathBuf),
    /// A mounted camera card, by its volume, with the name its row shows.
    Card { volume_id: VolumeId, name: String },
}

impl ReadSource {
    /// `index.refresh`'s `source`.
    pub(crate) fn refresh(&self) -> IndexSource {
        match self {
            Self::Folder(path) => IndexSource::Folder { path: path.clone() },
            Self::Card { volume_id, .. } => IndexSource::Card {
                volume_id: volume_id.clone(),
            },
        }
    }

    /// `index.refresh`'s parameters.
    pub(crate) fn refresh_params(&self) -> Value {
        json!({ "source": self.refresh() })
    }

    /// What the status bar calls it while it is read.
    pub(crate) fn name(&self, home: Option<&Path>) -> String {
        match self {
            Self::Folder(path) => shown_path(path, home),
            Self::Card { name, .. } => format!("the {name} card"),
        }
    }

    /// What the first look's progress sheet calls it: a folder's own name, or the card's.
    pub(crate) fn sheet_name(&self) -> String {
        match self {
            Self::Folder(path) => path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            Self::Card { name, .. } => name.clone(),
        }
    }
}

/// What pressing a source row does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SourcePress {
    View(ViewSource),
    /// A card or a folder on disk: read it, then view it.
    Read(ReadSource),
    /// A volume On disk: open or close it.
    Toggle(PathBuf),
    /// Browse a folder…: the native folder dialog.
    BrowseFolder,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceRow {
    pub(crate) icon: SourceIcon,
    pub(crate) name: String,
    pub(crate) secondary: Option<String>,
    pub(crate) count: Count,
    /// The volume dot, or an event's offline one.
    pub(crate) dot: Option<Dot>,
    /// Drawn in tertiary ink (Removed, an offline volume).
    pub(crate) dimmed: bool,
    pub(crate) selected: bool,
    /// Nesting On disk: a volume's folders one level in, theirs two.
    pub(crate) indent: u8,
    /// A row that opens: whether it is open, and the path its chevron opens or closes.
    pub(crate) disclosure: Option<(bool, PathBuf)>,
    /// What pressing it does; nothing for an offline volume.
    pub(crate) press: Option<SourcePress>,
}

impl SourceRow {
    /// A row with no count, dot, nesting or disclosure.
    fn plain(icon: SourceIcon, name: String, press: Option<SourcePress>) -> Self {
        Self {
            icon,
            name,
            secondary: None,
            count: Count::None,
            dot: None,
            dimmed: false,
            selected: false,
            indent: 0,
            disclosure: None,
            press,
        }
    }
}

/// Events under one month label.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MonthRows {
    pub(crate) label: String,
    pub(crate) rows: Vec<SourceRow>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SourcesModel {
    pub(crate) search: String,
    /// The mounted camera cards; the section is drawn only when there is one.
    pub(crate) cards: Vec<SourceRow>,
    pub(crate) months: Vec<MonthRows>,
    /// Said under Events in place of rows: reading, none, or unavailable.
    pub(crate) events_note: Option<String>,
    /// The volumes, each open one's folders under it, the folder browsed with Browse a folder…
    /// when it is not among them, and Browse a folder… itself.
    pub(crate) on_disk: Vec<SourceRow>,
    pub(crate) catalog: Vec<SourceRow>,
}

/// One item of a chip's or the sort's menu.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MenuChoice {
    pub(crate) label: String,
    /// A count.
    pub(crate) trailing: Option<String>,
    pub(crate) checked: bool,
    /// `None` draws the item disabled, with `label` saying why.
    pub(crate) change: Option<QueryChange>,
}

/// A filter chip: its label, whether its condition is set, and its menu while open.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ChipModel {
    pub(crate) label: String,
    pub(crate) set: bool,
    pub(crate) menu: Option<Vec<MenuChoice>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PickSegments {
    pub(crate) selected: PickFilter,
    /// Picked's count.
    pub(crate) picked: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FilterBarModel {
    /// A source is chosen, so the chips act.
    pub(crate) enabled: bool,
    /// Over files only.
    pub(crate) pick: Option<PickSegments>,
    pub(crate) camera: ChipModel,
    pub(crate) kind: ChipModel,
    /// Over files only: the Group chip.
    pub(crate) group: Option<ChipModel>,
    /// The view's size, at the bar's right.
    pub(crate) count: String,
}

/// The floating strip: Grid (Loupe comes later), the sort and the size slider.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StripModel {
    pub(crate) enabled: bool,
    pub(crate) sort: String,
    pub(crate) sort_menu: Option<Vec<MenuChoice>>,
    pub(crate) cell_width: f32,
}

/// One item's Info panel.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ItemInfo {
    pub(crate) name: String,
    /// The preview placeholder's shape.
    pub(crate) aspect: Option<f32>,
    /// The Pick band, for a file.
    pub(crate) pick: Option<PickBand>,
    /// The Moment band's rows, empty for a single or a photograph.
    pub(crate) moment: Vec<(String, String)>,
    pub(crate) metadata: Vec<(String, String)>,
}

/// The Info panel's Pick band for a file: "Picked for Develop" or "Pick", which `P` does too, and
/// once picked, what that means.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PickBand {
    pub(crate) picked: bool,
    pub(crate) note: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum InfoModel {
    /// Nothing selected.
    #[default]
    Nothing,
    /// The active item, whose row is not read yet.
    Reading,
    One(ItemInfo),
    /// Several selected: their count and the active item's name.
    Several {
        count: String,
        active: Option<String>,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SelectTitle {
    /// The view's name: the event, the folder or the catalog view.
    pub(crate) name: String,
    /// Its dates, size, cameras and where its files are.
    pub(crate) summary: String,
    /// Develop N's count: the picks in view.
    pub(crate) picks: u32,
    pub(crate) sources_open: bool,
    pub(crate) info_open: bool,
    /// The window fills the screen, so the bar starts at its ordinary inset.
    pub(crate) fullscreen: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SelectStatus {
    pub(crate) message: String,
    pub(crate) clients: String,
    pub(crate) agents_connected: bool,
    /// "Camera previews · auto-organized · N in view · M picked" over files, "N in view · M
    /// selected" over the catalog.
    pub(crate) line: String,
}

/// What the Select workspace shows, derived after every message. With Develop shown it is empty.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SelectModel {
    pub(crate) shown: Shown,
    pub(crate) title: SelectTitle,
    pub(crate) sources: SourcesModel,
    pub(crate) filter: FilterBarModel,
    pub(crate) strip: StripModel,
    pub(crate) selection: SelectionModel,
    pub(crate) info: InfoModel,
    pub(crate) status: SelectStatus,
    /// Said in the centre instead of the grid: choose a source, reading, nothing in view, a failure.
    pub(crate) note: Option<String>,
    /// The catalog's larger cells.
    pub(crate) catalog_cells: bool,
    /// The loupe, drawn in place of the grid while it is open.
    pub(crate) loupe: super::loupe::LoupeModel,
    /// Missing originals, drawn in place of the grid while it is the source.
    pub(crate) missing: super::select_missing::MissingModel,
    /// The catalog's folders and collections, and over the catalog its filter bar, Metadata
    /// browser and Info panel.
    pub(crate) catalog: super::select_catalog::CatalogModel,
}

/// The Select workspace's model, derived from the inputs.
pub(crate) fn derive(inputs: &Inputs<'_>) -> SelectModel {
    model(
        inputs.select,
        &inputs.session.browse,
        inputs.status,
        inputs.clients,
        inputs.view_state.fullscreen,
    )
}

/// The model from its parts. With Develop shown nothing is derived.
pub(crate) fn model(
    state: &SelectState,
    browse: &BrowseSession,
    status: &str,
    clients: Option<usize>,
    fullscreen: bool,
) -> SelectModel {
    if state.shown == Shown::Develop {
        return SelectModel::default();
    }
    let selection = SelectionModel::of(browse, state.revision());
    let catalog = super::select_catalog::derive(state, &selection, status);
    let mut model = SelectModel {
        shown: state.shown,
        title: SelectTitle {
            fullscreen,
            ..title(state)
        },
        sources: sources(state),
        filter: filter_bar(state),
        strip: strip(state),
        info: info(state, &selection),
        status: SelectStatus {
            message: status.to_owned(),
            clients: clients_text(clients),
            agents_connected: clients.is_some_and(|count| count > 0),
            line: status_line(state, &selection),
        },
        selection,
        note: note(state),
        catalog_cells: state.over_catalog(),
        loupe: super::loupe::derive(state, browse),
        missing: super::select_missing::derive(state),
        catalog,
    };
    // The loupe says what it shows and whether its next frames are ready.
    if model.loupe.open && !model.loupe.status.is_empty() {
        model.status.line = model.loupe.status.clone();
    }
    super::select_missing::over(model)
}

/// The source being viewed, or asked for.
fn source(state: &SelectState) -> Option<&ViewSource> {
    state.query.as_ref().map(|query| &query.source)
}

/// The event the view's source names, from the event list.
fn event<'a>(state: &'a SelectState, source: &ViewSource) -> Option<&'a Event> {
    let ViewSource::Event { event_id } = source else {
        return None;
    };
    state
        .events
        .as_ref()?
        .events
        .iter()
        .find(|event| &event.id == event_id)
}

/// The summary, when it describes the source on screen.
fn summary_of(state: &SelectState) -> Option<&ViewSummary> {
    state
        .summary
        .as_ref()
        .filter(|summary| Some(&summary.query.source) == source(state))
}

pub(crate) fn title(state: &SelectState) -> SelectTitle {
    let panels = |mut title: SelectTitle| {
        title.sources_open = state.sources_panel;
        title.info_open = state.info_panel;
        title
    };
    // The view that waits for its card or folder to be read is the one on screen.
    if let Some(reading) = &state.reading {
        let mut parts = vec!["Reading\u{2026}".to_owned()];
        if let ReadSource::Folder(path) = reading {
            parts.push(shown_path(path, state.home.as_deref()));
        }
        return panels(SelectTitle {
            name: reading.sheet_name(),
            summary: parts.join(" \u{b7} "),
            ..SelectTitle::default()
        });
    }
    let Some(source) = source(state) else {
        return panels(SelectTitle {
            name: "Select".to_owned(),
            ..SelectTitle::default()
        });
    };
    let summary = summary_of(state);
    let count = summary.map(|summary| summary.count);
    let mut parts = Vec::new();
    let name = match source {
        ViewSource::Event { .. } => match event(state, source) {
            Some(event) => {
                if let Some(dates) = dates(event.first_day, event.last_day, true) {
                    parts.push(dates);
                }
                parts.push(photographs(count.unwrap_or(event.count)));
                if !event.cameras.is_empty() {
                    parts.push(event.cameras.join(", "));
                }
                if !event.roots.is_empty() {
                    let roots: Vec<String> = event
                        .roots
                        .iter()
                        .map(|root| shown_path(root, state.home.as_deref()))
                        .collect();
                    parts.push(format!("from {}", and_list(&roots)));
                }
                event_label(event)
            }
            None => {
                parts.extend(count.map(photographs));
                "Event".to_owned()
            }
        },
        ViewSource::Folder { path, .. } => {
            parts.extend(count.map(photographs));
            parts.push(shown_path(path, state.home.as_deref()));
            file_name(path)
        }
        ViewSource::Card { .. } => {
            parts.extend(count.map(photographs));
            "Card".to_owned()
        }
        other => {
            parts.extend(count.map(|count| {
                if count == 1 {
                    "1 developed photograph".to_owned()
                } else {
                    format!("{} developed photographs", thousands(count))
                }
            }));
            // A catalog folder or collection by its own name, once its list is read.
            super::select_catalog::source_name(&state.catalog, other)
                .unwrap_or_else(|| catalog_name(other).to_owned())
        }
    };
    panels(SelectTitle {
        name,
        summary: parts.join(" \u{b7} "),
        picks: summary
            .filter(|summary| summary.query.source.over_files())
            .map_or(0, |summary| summary.picked),
        ..SelectTitle::default()
    })
}

fn catalog_name(source: &ViewSource) -> &'static str {
    match source {
        ViewSource::AllPhotographs => "All photographs",
        ViewSource::RecentlyDeveloped { .. } => "Recently developed",
        ViewSource::CatalogFolder { .. } => "Catalog folder",
        ViewSource::Collection { .. } => "Collection",
        ViewSource::MissingOriginals => "Missing originals",
        ViewSource::Removed => "Removed",
        ViewSource::Event { .. } => "Event",
        ViewSource::Folder { .. } => "Folder",
        ViewSource::Card { .. } => "Card",
    }
}

pub(crate) fn sources(state: &SelectState) -> SourcesModel {
    let selected = source(state);
    let is = |source: &ViewSource| selected == Some(source);
    let mut months = Vec::new();
    let mut events_note = None;
    match (&state.events, &state.events_error) {
        (_, Some(_)) => events_note = Some("Events unavailable".to_owned()),
        (None, None) => events_note = Some("Reading events\u{2026}".to_owned()),
        (Some(list), None) => {
            for month in &list.months {
                let rows: Vec<SourceRow> = list
                    .events
                    .iter()
                    .filter(|event| event.months.contains(&month.month))
                    .map(|event| event_row(event, &is))
                    .collect();
                if !rows.is_empty() {
                    months.push(MonthRows {
                        label: month_label(month.month),
                        rows,
                    });
                }
            }
            let undated: Vec<SourceRow> = list
                .events
                .iter()
                .filter(|event| event.months.is_empty())
                .map(|event| event_row(event, &is))
                .collect();
            if !undated.is_empty() {
                months.push(MonthRows {
                    label: "Undated".to_owned(),
                    rows: undated,
                });
            }
            if months.is_empty() {
                events_note = Some(if state.search.trim().is_empty() {
                    "No events".to_owned()
                } else {
                    "No events match".to_owned()
                });
            }
        }
    }
    let catalog = catalog_rows(state, &is);
    SourcesModel {
        search: state.search.clone(),
        cards: card_rows(state, &is),
        months,
        events_note,
        on_disk: disk_rows(state, selected),
        catalog,
    }
}

/// Cards: each mounted card by its volume's name, with the first camera its files were read from
/// and how many files its last listing found. Pressing one reads it and views it.
fn card_rows(state: &SelectState, is: &impl Fn(&ViewSource) -> bool) -> Vec<SourceRow> {
    let Some(cards) = &state.cards else {
        return Vec::new();
    };
    cards
        .cards
        .iter()
        .map(|card| {
            let name = card.volume.label.clone();
            let camera = card
                .cameras
                .first()
                .filter(|camera| **camera != name)
                .cloned();
            SourceRow {
                secondary: camera,
                count: card
                    .files
                    .map_or(Count::None, |files| Count::Total(thousands(files))),
                selected: is(&ViewSource::Card {
                    volume_id: card.volume.id.clone(),
                }),
                ..SourceRow::plain(
                    SourceIcon::Drive,
                    name.clone(),
                    Some(SourcePress::Read(ReadSource::Card {
                        volume_id: card.volume.id.clone(),
                        name,
                    })),
                )
            }
        })
        .collect()
}

/// On disk: every volume but the cards (listed under Cards), the startup disk first, each with its
/// dot; a mounted one opens to its folders, and each folder opens to its own, as `disk.folders`
/// answered them. Pressing a volume opens or closes it; pressing a folder reads it with its
/// subfolders and views it. The folder Browse a folder… chose follows when it is not among them,
/// then Browse a folder… itself, for a folder the volumes do not reach.
fn disk_rows(state: &SelectState, selected: Option<&ViewSource>) -> Vec<SourceRow> {
    let viewing = |path: &Path| matches!(selected, Some(ViewSource::Folder { path: viewed, .. }) if viewed == path);
    let mut rows = Vec::new();
    for listed in state
        .volumes
        .iter()
        .flat_map(|volumes| &volumes.volumes)
        .filter(|listed| !listed.card)
    {
        let mount = &listed.volume.mount_point;
        let open = !listed.offline && state.open.contains(mount);
        rows.push(SourceRow {
            dot: Some(if listed.offline {
                Dot::Offline
            } else {
                Dot::Mounted
            }),
            dimmed: listed.offline,
            disclosure: (!listed.offline).then(|| (open, mount.clone())),
            ..SourceRow::plain(
                SourceIcon::Drive,
                listed.volume.label.clone(),
                (!listed.offline).then(|| SourcePress::Toggle(mount.clone())),
            )
        });
        if open {
            folder_rows(state, mount, 1, &viewing, &mut rows);
        }
    }
    if let Some(folder) = &state.folder
        && !rows.iter().any(|row| {
            matches!(&row.press, Some(SourcePress::Read(ReadSource::Folder(path))) if path == folder)
        })
    {
        rows.push(SourceRow {
            selected: viewing(folder),
            ..SourceRow::plain(
                SourceIcon::Folder,
                file_name(folder),
                Some(SourcePress::View(ViewSource::Folder {
                    path: folder.clone(),
                    subfolders: true,
                })),
            )
        });
    }
    rows.push(SourceRow::plain(
        SourceIcon::Folder,
        "Browse a folder\u{2026}".to_owned(),
        Some(SourcePress::BrowseFolder),
    ));
    rows
}

/// The folders under `parent` that `disk.folders` answered, each followed by its own when open.
/// Nesting is bounded by what a person opens, and each listing by `disk.folders`' own bound.
fn folder_rows(
    state: &SelectState,
    parent: &Path,
    indent: u8,
    viewing: &impl Fn(&Path) -> bool,
    rows: &mut Vec<SourceRow>,
) {
    let Some(listed) = state.disk.get(parent) else {
        return;
    };
    for folder in &listed.folders {
        let open = state.open.contains(&folder.path);
        rows.push(SourceRow {
            selected: viewing(&folder.path),
            indent,
            disclosure: Some((open, folder.path.clone())),
            ..SourceRow::plain(
                SourceIcon::Folder,
                folder.name.clone(),
                Some(SourcePress::Read(ReadSource::Folder(folder.path.clone()))),
            )
        });
        if open {
            folder_rows(state, &folder.path, indent.saturating_add(1), viewing, rows);
        }
    }
}

/// A count as the sources panel writes it.
fn count_text(count: u64) -> String {
    thousands(count.min(u64::from(u32::MAX)) as u32)
}

/// Catalog: All photographs, Recently developed, Missing originals and Removed, with the counts
/// `catalog.info` answered: Missing originals' in the clipping red while there are any, Removed
/// dimmed.
fn catalog_rows(state: &SelectState, is: &impl Fn(&ViewSource) -> bool) -> Vec<SourceRow> {
    let counts = state.counts.as_ref();
    let total = |count: fn(&CatalogCounts) -> u64| {
        counts.map_or(Count::None, |counts| {
            Count::Total(count_text(count(counts)))
        })
    };
    [
        (
            SourceIcon::AllPhotographs,
            ViewSource::AllPhotographs,
            total(|counts| counts.photographs),
        ),
        (
            SourceIcon::Recent,
            ViewSource::RecentlyDeveloped {
                days: luxforge_core::catalog_types::DEFAULT_RECENT_DAYS,
            },
            total(|counts| counts.recently_developed),
        ),
        (
            SourceIcon::Missing,
            ViewSource::MissingOriginals,
            match counts.map(|counts| counts.unavailable) {
                Some(missing) if missing > 0 => Count::Unavailable(count_text(missing)),
                _ => Count::None,
            },
        ),
        (
            SourceIcon::Removed,
            ViewSource::Removed,
            total(|counts| counts.removed),
        ),
    ]
    .into_iter()
    .map(|(icon, source, count)| SourceRow {
        count,
        dimmed: icon == SourceIcon::Removed,
        selected: is(&source),
        ..SourceRow::plain(
            icon,
            catalog_name(&source).to_owned(),
            Some(SourcePress::View(source)),
        )
    })
    .collect()
}

/// What an event is called in the sources panel and the title bar: its name without the dates it
/// ends with, which the row and the summary give beside it, so "Konstanz · 12–13 Sep" reads
/// "Konstanz", and an Undated event's without the "Undated · " its section already says. Any other
/// name is kept whole.
pub(crate) fn event_label(event: &Event) -> String {
    // An Undated event is listed under Undated, so its row names its folder alone.
    if event.undated
        && let Some(folder) = event.label.strip_prefix("Undated \u{b7} ")
        && !folder.trim().is_empty()
    {
        return folder.to_owned();
    }
    // A name that is its dates alone has no label: the whole name, then.
    if event.label.trim().is_empty() {
        return event.name.clone();
    }
    event.label.clone()
}

fn event_row(event: &Event, is: &impl Fn(&ViewSource) -> bool) -> SourceRow {
    let source = ViewSource::Event {
        event_id: event.id.clone(),
    };
    SourceRow {
        secondary: dates(event.first_day, event.last_day, false),
        count: if event.picked > 0 {
            Count::Picks {
                picked: thousands(event.picked),
                total: thousands(event.count),
            }
        } else {
            Count::Total(thousands(event.count))
        },
        dot: (event.offline > 0).then_some(Dot::Offline),
        selected: is(&source),
        ..SourceRow::plain(
            SourceIcon::Event,
            event_label(event),
            Some(SourcePress::View(source)),
        )
    }
}

pub(crate) fn filter_bar(state: &SelectState) -> FilterBarModel {
    let Some(query) = &state.query else {
        return FilterBarModel {
            camera: ChipModel {
                label: "Camera".to_owned(),
                ..ChipModel::default()
            },
            kind: ChipModel {
                label: "Kind".to_owned(),
                ..ChipModel::default()
            },
            ..FilterBarModel::default()
        };
    };
    let over_files = query.source.over_files();
    let facets = state.facets.as_ref();
    let values = |facet: Facet| facets.and_then(|facets| facets.counts.get(&facet));
    let menu_open = |menu: SelectMenu| state.menu == Some(menu);

    // Camera: every body the facets count, by its label.
    let cameras = values(Facet::Camera);
    let camera_label = |key: &BodyKey| {
        cameras
            .and_then(|values| {
                values
                    .iter()
                    .find(|value| value.value.as_deref().unwrap_or_default() == key.0)
            })
            .and_then(|value| value.label.clone().or_else(|| value.value.clone()))
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| "Unknown camera".to_owned())
    };
    let camera = ChipModel {
        label: match query.filter.cameras.as_slice() {
            [] => "Camera".to_owned(),
            [one] => camera_label(one),
            several => format!("{} cameras", several.len()),
        },
        set: !query.filter.cameras.is_empty(),
        menu: menu_open(SelectMenu::Camera).then(|| {
            let mut choices = vec![MenuChoice {
                label: "All cameras".to_owned(),
                trailing: None,
                checked: query.filter.cameras.is_empty(),
                change: Some(QueryChange::Camera(None)),
            }];
            match cameras {
                Some(values) => choices.extend(values.iter().map(|value| {
                    let key = BodyKey(value.value.clone().unwrap_or_default());
                    MenuChoice {
                        label: camera_label(&key),
                        trailing: Some(thousands(value.count)),
                        checked: query.filter.cameras == [key.clone()],
                        change: Some(QueryChange::Camera(Some(key))),
                    }
                })),
                None => choices.push(MenuChoice {
                    label: "Cameras unavailable".to_owned(),
                    trailing: None,
                    checked: false,
                    change: None,
                }),
            }
            choices
        }),
    };

    // Kind: JPEG and RAW, counted when the facets say.
    let kind_count = |tag: SourceTag| {
        values(Facet::Kind).and_then(|values| {
            values
                .iter()
                .find(|value| value.value.as_deref() == Some(kind_value(tag)))
                .map(|value| thousands(value.count))
        })
    };
    let kind = ChipModel {
        label: match query.filter.kinds.as_slice() {
            [one] => one.label().to_owned(),
            _ => "Kind".to_owned(),
        },
        set: !query.filter.kinds.is_empty(),
        menu: menu_open(SelectMenu::Kind).then(|| {
            let mut choices = vec![MenuChoice {
                label: "All kinds".to_owned(),
                trailing: None,
                checked: query.filter.kinds.is_empty(),
                change: Some(QueryChange::Kind(None)),
            }];
            choices.extend([SourceTag::Raw, SourceTag::Jpeg].map(|tag| MenuChoice {
                label: tag.label().to_owned(),
                trailing: kind_count(tag),
                checked: query.filter.kinds == [tag],
                change: Some(QueryChange::Kind(Some(tag))),
            }));
            choices
        }),
    };

    let group = over_files.then(|| ChipModel {
        label: format!("Group: {}", grouping_label(query.grouping)),
        set: false,
        menu: menu_open(SelectMenu::Group).then(|| {
            Grouping::ALL
                .map(|grouping| MenuChoice {
                    label: grouping_label(grouping).to_owned(),
                    trailing: None,
                    checked: query.grouping == grouping,
                    change: Some(QueryChange::Group(grouping)),
                })
                .to_vec()
        }),
    });

    let summary = summary_of(state);
    let pick = over_files.then(|| PickSegments {
        selected: pick_filter(query),
        picked: picked_count(state),
    });
    FilterBarModel {
        enabled: true,
        pick,
        camera,
        kind,
        group,
        count: match summary {
            Some(summary) => photographs(summary.count),
            None if state.loading => "Reading\u{2026}".to_owned(),
            None => String::new(),
        },
    }
}

/// Picked's count: the Pick facet's, which counts picks whatever the pick segment chosen, or the
/// summary's while every frame is shown.
fn picked_count(state: &SelectState) -> Option<String> {
    let from_facets = state
        .facets
        .as_ref()
        .and_then(|facets| facets.counts.get(&Facet::Pick))
        .and_then(|values| {
            values
                .iter()
                .find(|value| value.value.as_deref() == Some("picked"))
        })
        .map(|value| value.count);
    let from_summary = summary_of(state)
        .filter(|summary| pick_filter(&summary.query) == PickFilter::All)
        .map(|summary| summary.picked);
    from_facets
        .or(from_summary)
        .filter(|&count| count > 0)
        .map(thousands)
}

/// A kind as the Kind facet names it.
fn kind_value(tag: SourceTag) -> &'static str {
    match tag {
        SourceTag::Jpeg => "jpeg",
        SourceTag::Raw => "raw",
    }
}

pub(crate) fn strip(state: &SelectState) -> StripModel {
    let Some(query) = &state.query else {
        return StripModel {
            enabled: false,
            sort: sort_label(SortKey::CaptureTime).to_owned(),
            sort_menu: None,
            cell_width: state.cell_width(),
        };
    };
    StripModel {
        enabled: true,
        sort: sort_label(query.sort.key).to_owned(),
        sort_menu: (state.menu == Some(SelectMenu::Sort)).then(|| {
            sorts(query.source.over_files())
                .iter()
                .map(|&key| MenuChoice {
                    label: sort_label(key).to_owned(),
                    trailing: None,
                    checked: query.sort.key == key,
                    change: Some(QueryChange::Sort(key)),
                })
                .collect()
        }),
        cell_width: state.cell_width(),
    }
}

pub(crate) fn info(state: &SelectState, selection: &SelectionModel) -> InfoModel {
    let focus = selection.focus();
    if selection.count > 1 {
        return InfoModel::Several {
            count: format!("{} selected", thousands(selection.count)),
            active: focus
                .and_then(|item| state.rows.row(item))
                .map(|row| row.file_name.clone()),
        };
    }
    let Some(item) = focus else {
        return InfoModel::Nothing;
    };
    match state.rows.row(item) {
        Some(row) => {
            let mut info = item_info(row, state.summary.as_ref(), state.home.as_deref());
            if matches!(row.item, RowItem::File { .. }) {
                let picks = title(state).picks;
                info.pick = Some(PickBand {
                    picked: row.picked,
                    note: row.picked.then(|| {
                        format!(
                            "Joins the catalog when you press Develop {}. The file stays where it is.",
                            thousands(picks)
                        )
                    }),
                });
            }
            InfoModel::One(info)
        }
        None => InfoModel::Reading,
    }
}

/// One item's Moment and Metadata bands, from its row and the summary's moments.
pub(crate) fn item_info(
    row: &ViewRow,
    summary: Option<&ViewSummary>,
    home: Option<&Path>,
) -> ItemInfo {
    let mut moment = Vec::new();
    if let Some(at) = row.moment
        && let Some(group) =
            summary.and_then(|summary| summary.groups.moments.get(at.index as usize))
        && group.kind != MomentKind::Single
    {
        let kind = if group.kind == MomentKind::Bracket {
            "Bracket"
        } else {
            "Burst"
        };
        moment.push((
            "Moment".to_owned(),
            format!("{kind} \u{b7} frame {} of {}", at.frame + 1, group.len),
        ));
        moment.push(("Why".to_owned(), why(group)));
        moment.push(("Frames".to_owned(), moment_detail(group)));
    }
    let mut metadata = Vec::new();
    let mut push = |label: &str, value: Option<String>| {
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            metadata.push((label.to_owned(), value));
        }
    };
    push("Captured", row.capture.clone());
    push("Place", row.place.clone());
    push("Camera", row.camera.clone());
    push("Lens", row.lens.clone());
    push("Exposure", exposure_text(&row.exposure));
    push(
        "Dimensions",
        Some(match row.dimensions {
            Some(dimensions) => {
                let (width, height) = if row.orientation.is_some_and(|turn| turn.transposes()) {
                    (dimensions.height, dimensions.width)
                } else {
                    (dimensions.width, dimensions.height)
                };
                format!("{width} \u{d7} {height} \u{b7} {}", row.kind.label())
            }
            None => row.kind.label().to_owned(),
        }),
    );
    push("File", Some(row.file_name.clone()));
    push(
        "Folder",
        row.path
            .parent()
            .map(|folder| shown_path(folder, home))
            .filter(|folder| !folder.is_empty()),
    );
    push(
        "Original",
        match row.availability {
            FileAvailability::Available => None,
            FileAvailability::Offline => Some("Offline".to_owned()),
            FileAvailability::Missing => Some("Missing".to_owned()),
            FileAvailability::Changed => Some("Changed since it was read".to_owned()),
        },
    );
    match row.item {
        RowItem::File { .. } if row.developed_as.is_some() => {
            push("Catalog", Some("In the catalog".to_owned()));
        }
        RowItem::Photo { .. } => {
            push(
                "Edited",
                Some(if row.edited { "Yes" } else { "No" }.to_owned()),
            );
        }
        RowItem::File { .. } => {}
    }
    ItemInfo {
        name: row.file_name.clone(),
        aspect: aspect(row),
        pick: None,
        moment,
        metadata,
    }
}

/// Why a moment is one: a burst's frames are close together, a bracket's exposure steps.
fn why(moment: &Moment) -> String {
    match (moment.kind, moment.evidence) {
        (MomentKind::Bracket, Some(BracketEvidence::Previews)) => {
            "Brightness steps, measured on the previews".to_owned()
        }
        (MomentKind::Bracket, _) => "Exposure steps, from the metadata".to_owned(),
        _ => {
            let gaps = moment.len.saturating_sub(1).max(1);
            format!(
                "About {} apart",
                span_text(moment.span_ms / u64::from(gaps))
            )
        }
    }
}

/// An exposure as the Info panel says it: `1/2000 s · f/5.6 · ISO 100 · +0.7 EV · 28 mm`.
pub(crate) fn exposure_text(exposure: &Exposure) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(time) = exposure.time_s.filter(|time| *time > 0.0) {
        parts.push(if time < 1.0 {
            format!("1/{} s", (1.0 / time).round() as u64)
        } else if time.fract() == 0.0 {
            format!("{} s", time as u64)
        } else {
            format!("{time:.1} s")
        });
    }
    if let Some(f_number) = exposure.f_number.filter(|value| *value > 0.0) {
        parts.push(format!("f/{}", tenths(f_number)));
    }
    if let Some(iso) = exposure.iso {
        parts.push(format!("ISO {iso}"));
    }
    if let Some(bias) = exposure.bias_ev.filter(|bias| (bias * 10.0).round() != 0.0) {
        parts.push(format!("{} EV", ev_step(bias)));
    }
    if let Some(focal) = exposure.focal_mm.filter(|value| *value > 0.0) {
        parts.push(format!("{} mm", tenths(focal)));
    }
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// A value to a tenth, without a trailing `.0`.
fn tenths(value: f32) -> String {
    let tenths = (value * 10.0).round() / 10.0;
    if tenths.fract() == 0.0 {
        format!("{}", tenths as i64)
    } else {
        format!("{tenths:.1}")
    }
}

fn status_line(state: &SelectState, selection: &SelectionModel) -> String {
    let files = !state.over_catalog();
    let Some(summary) = summary_of(state) else {
        return if files {
            "Camera previews \u{b7} auto-organized".to_owned()
        } else {
            String::new()
        };
    };
    if files {
        format!(
            "Camera previews \u{b7} auto-organized \u{b7} {} in view \u{b7} {} picked",
            thousands(summary.count),
            thousands(summary.picked)
        )
    } else {
        format!(
            "{} in view \u{b7} {} selected",
            thousands(summary.count),
            thousands(selection.count)
        )
    }
}

fn note(state: &SelectState) -> Option<String> {
    if state.query.is_none() {
        return Some("Choose an event, a folder on disk or a catalog view".to_owned());
    }
    if let Some(error) = &state.view_error {
        return Some(error.clone());
    }
    match summary_of(state) {
        None => Some("Reading the view\u{2026}".to_owned()),
        Some(summary) if summary.count == 0 => Some("Nothing in this view".to_owned()),
        Some(_) => None,
    }
}

// -- Words and numbers -----------------------------------------------------------------------------

/// `n` with its thousands grouped by commas, as every count on the boards is written: `1,042`.
pub(crate) fn thousands(n: u32) -> String {
    let digits = n.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// `1 photograph`, `1,042 photographs`.
pub(crate) fn photographs(count: u32) -> String {
    if count == 1 {
        "1 photograph".to_owned()
    } else {
        format!("{} photographs", thousands(count))
    }
}

const MONTHS: [&str; 12] = [
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

const WEEKDAYS: [&str; 7] = [
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
    "Sunday",
];

fn month_name(month: u32) -> &'static str {
    MONTHS[(month.clamp(1, 12) - 1) as usize]
}

fn short_month(month: u32) -> &'static str {
    &month_name(month)[..3]
}

/// `September 2026`.
pub(crate) fn month_label(month: Month) -> String {
    format!("{} {}", month_name(month.month), month.year)
}

/// A day heading: `Saturday 12 September 2026`, or `Undated`.
pub(crate) fn day_title(day: Option<LocalDay>) -> String {
    let Some(day) = day else {
        return "Undated".to_owned();
    };
    let (year, month, date) = day.ymd();
    // Day 0 is Thursday 1 January 1970.
    let weekday = WEEKDAYS[(day.0 + 3).rem_euclid(7) as usize];
    format!("{weekday} {date} {} {year}", month_name(month))
}

/// An event's dates: `14 Sep`, `12–13 Sep`, `30 Sep – 2 Oct`, with the year when asked for.
pub(crate) fn dates(first: Option<LocalDay>, last: Option<LocalDay>, year: bool) -> Option<String> {
    let first = first?;
    let (y1, m1, d1) = first.ymd();
    let (y2, m2, d2) = last.unwrap_or(first).ymd();
    let text = if (y1, m1, d1) == (y2, m2, d2) {
        format!("{d1} {}", short_month(m1))
    } else if (y1, m1) == (y2, m2) {
        format!("{d1}\u{2013}{d2} {}", short_month(m1))
    } else if y1 == y2 {
        format!("{d1} {} \u{2013} {d2} {}", short_month(m1), short_month(m2))
    } else {
        return Some(format!(
            "{d1} {} {y1} \u{2013} {d2} {} {y2}",
            short_month(m1),
            short_month(m2)
        ));
    };
    Some(if year { format!("{text} {y2}") } else { text })
}

/// A path as the desktop shows it, under the home folder as `~`.
pub(crate) fn shown_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(rest) = path.strip_prefix(home)
    {
        return if rest.as_os_str().is_empty() {
            "~".to_owned()
        } else {
            format!("~/{}", rest.display())
        };
    }
    path.display().to_string()
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `a`, `a and b`, `a, b and c`.
fn and_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
