//! The Select workspace's view model ([catalog design](../../../../docs/design/catalog.md#workspaces)):
//! which workspace the window shows, what the Select workspace last read from the owner — the event
//! list, the view's summary and facets, and a bounded window of its rows — and the plain data its
//! regions are drawn from: the title bar, the sources panel, the filter bar and the floating strip,
//! the grid's blocks and cells, the Info panel and the status line. **Lane D (views and desktop)**
//! owns it. Like every view model it names no framework type, no widget and no view.
//!
//! The desktop holds no catalog logic. A change of source, filter, sort or grouping is a whole
//! [`ViewQuery`] for the owner to evaluate ([`changed`]); a selection gesture is the `browse.select`
//! request an API client would send ([`select_params`]); the grid's blocks are the summary's group
//! layout as the owner answered it ([`grid_content`]).
use super::{Inputs, status::clients_text};
use luxforge_core::{
    SourceTag,
    catalog_types::{
        BodyKey, BracketEvidence, BrowseSession, Event, EventList, Exposure, Facet, Facets,
        FileAvailability, Grouping, LocalDay, Moment, MomentKind, Month, PreviewState, RowItem,
        SortKey, ViewQuery, ViewRow, ViewSource, ViewSummary,
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
    /// The size slider's cell widths, one per preset.
    pub(crate) files_cell_width: f32,
    pub(crate) catalog_cell_width: f32,
    /// The home folder, which paths are shown under as `~`.
    pub(crate) home: Option<PathBuf>,
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
            query: None,
            loading: false,
            summary: None,
            view_error: None,
            facets: None,
            rows: RowCache::default(),
            content: GridContent::default(),
            collapsed: BTreeSet::new(),
            menu: None,
            files_cell_width: FILES_CELL_WIDTH,
            catalog_cell_width: CATALOG_CELL_WIDTH,
            home: None,
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
    /// A burst or a bracket of `frames` consecutive items, with its header.
    Moment {
        bracket: bool,
        title: String,
        detail: String,
        evidence: Option<String>,
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
/// with no moments is all singles. `collapsed` names the bursts drawn as one cell.
pub(crate) fn grid_content(summary: &ViewSummary, collapsed: &BTreeSet<u32>) -> GridContent {
    let count = summary.count;
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
            content.blocks.push(Block::Day {
                title: day_title(group.day),
                detail: photographs(group.len.min(count - position)),
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
                    bracket,
                    title: if bracket { "Bracket" } else { "Burst" }.to_owned(),
                    detail: moment_detail(group),
                    evidence: group.evidence.map(evidence_label).map(str::to_owned),
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
#[derive(Clone, Debug, Default)]
pub(crate) struct RowCache {
    revision: u64,
    count: u32,
    blocks: BTreeMap<u32, Vec<ViewRow>>,
    in_flight: Option<u32>,
    failed: BTreeSet<u32>,
}

impl RowCache {
    /// Forget every row: the view is now `count` items at `revision`.
    pub(crate) fn reset(&mut self, revision: u64, count: u32) {
        *self = Self {
            revision,
            count,
            ..Self::default()
        };
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

    /// The block a request is reading, if one is in flight.
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> Option<u32> {
        self.in_flight
    }

    pub(crate) fn row(&self, position: u32) -> Option<&ViewRow> {
        self.blocks
            .get(&(position / ROW_BLOCK))?
            .get((position % ROW_BLOCK) as usize)
    }

    /// What the grid draws for the cell whose first item is `position`, once its row is read.
    pub(crate) fn cell(&self, position: u32) -> Option<CellFacts> {
        self.row(position).map(cell_facts)
    }

    /// The next block to read for `wanted` items, counted in flight until it is answered; `None`
    /// while one is in flight or every wanted block is held or refused.
    pub(crate) fn next_request(&mut self, wanted: Range<u32>) -> Option<RowsRequest> {
        if self.in_flight.is_some() {
            return None;
        }
        let block = wanted_blocks(wanted, self.count)
            .find(|block| !self.blocks.contains_key(block) && !self.failed.contains(block))?;
        self.in_flight = Some(block);
        let from = block * ROW_BLOCK;
        Some(RowsRequest {
            revision: self.revision,
            from,
            count: ROW_BLOCK.min(self.count - from),
        })
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
        if self.in_flight == Some(block) {
            self.in_flight = None;
        }
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
        if self.in_flight == Some(block) {
            self.in_flight = None;
        }
        self.failed.insert(block);
    }
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
}

/// What pressing a source row does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SourcePress {
    View(ViewSource),
    /// Browse a folder…: the native folder dialog.
    BrowseFolder,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceRow {
    pub(crate) icon: SourceIcon,
    pub(crate) name: String,
    pub(crate) secondary: Option<String>,
    pub(crate) count: Count,
    /// Its files are offline: the hollow dot.
    pub(crate) offline: bool,
    /// Drawn in tertiary ink (Removed).
    pub(crate) dimmed: bool,
    pub(crate) selected: bool,
    pub(crate) press: SourcePress,
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
    pub(crate) months: Vec<MonthRows>,
    /// Said under Events in place of rows: reading, none, or unavailable.
    pub(crate) events_note: Option<String>,
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
    /// The Moment band's rows, empty for a single or a photograph.
    pub(crate) moment: Vec<(String, String)>,
    pub(crate) metadata: Vec<(String, String)>,
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
    SelectModel {
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
    }
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
                event.name.clone()
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
            catalog_name(other).to_owned()
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
    let mut on_disk = vec![SourceRow {
        icon: SourceIcon::Folder,
        name: "Browse a folder\u{2026}".to_owned(),
        secondary: None,
        count: Count::None,
        offline: false,
        dimmed: false,
        selected: false,
        press: SourcePress::BrowseFolder,
    }];
    if let Some(folder) = &state.folder {
        let source = ViewSource::Folder {
            path: folder.clone(),
            subfolders: true,
        };
        on_disk.push(SourceRow {
            icon: SourceIcon::Folder,
            name: file_name(folder),
            secondary: None,
            count: Count::None,
            offline: false,
            dimmed: false,
            selected: matches!(selected, Some(ViewSource::Folder { path, .. }) if path == folder),
            press: SourcePress::View(source),
        });
    }
    let catalog = [
        (SourceIcon::AllPhotographs, ViewSource::AllPhotographs),
        (
            SourceIcon::Recent,
            ViewSource::RecentlyDeveloped {
                days: luxforge_core::catalog_types::DEFAULT_RECENT_DAYS,
            },
        ),
        (SourceIcon::Missing, ViewSource::MissingOriginals),
        (SourceIcon::Removed, ViewSource::Removed),
    ]
    .into_iter()
    .map(|(icon, source)| SourceRow {
        icon,
        name: catalog_name(&source).to_owned(),
        secondary: None,
        count: Count::None,
        offline: false,
        dimmed: icon == SourceIcon::Removed,
        selected: is(&source),
        press: SourcePress::View(source),
    })
    .collect();
    SourcesModel {
        search: state.search.clone(),
        months,
        events_note,
        on_disk,
        catalog,
    }
}

fn event_row(event: &Event, is: &impl Fn(&ViewSource) -> bool) -> SourceRow {
    let source = ViewSource::Event {
        event_id: event.id.clone(),
    };
    SourceRow {
        icon: SourceIcon::Event,
        name: event.name.clone(),
        secondary: dates(event.first_day, event.last_day, false),
        count: if event.picked > 0 {
            Count::Picks {
                picked: thousands(event.picked),
                total: thousands(event.count),
            }
        } else {
            Count::Total(thousands(event.count))
        },
        offline: event.offline > 0,
        dimmed: false,
        selected: is(&source),
        press: SourcePress::View(source),
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
        Some(row) => InfoModel::One(item_info(
            row,
            state.summary.as_ref(),
            state.home.as_deref(),
        )),
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
