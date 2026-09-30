//! Views: what a client asks to see (`ViewQuery`), what the owner answers about the ordered list it
//! holds for that client (`ViewSummary`, `ViewRow`), counts (`Facets`) and the selection.
//!
//! A view is the client's one ordered list of files or photographs, held by the owner as
//! [`ViewItem`](super::ViewItem)s, 16 bytes each; `browse.view` evaluates a query into it,
//! `browse.rows` reads windows of it by position, and `browse.select` selects in it. Queries are
//! structured JSON, never SQL. A smart collection stores exactly a [`ViewQuery`].
use super::{
    BodyKey, CatalogFolderId, CollectionId, Dimensions, EventId, ExifOrientation, Exposure,
    FileAvailability, FileId, GroupLayout, Grouping, LibraryChangeSeq, LocalDay, Thresholds,
    VolumeId,
};
use crate::{AssetId, Error, SourceTag};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// The most items one view holds: 16 MB of [`ViewItem`](super::ViewItem)s, the design's structural
/// ceiling of 1,000,000 photographs. A query past it is refused with `resource-limit`.
pub const MAX_VIEW_ITEMS: usize = 1_000 * 1_000;
/// The most rows one `browse.rows` answers.
pub const MAX_VIEW_ROWS: usize = 1000;
/// The longest search text a filter carries, in characters.
pub const MAX_FILTER_TEXT: usize = 256;
/// Recently developed covers this many days when the source names none.
pub const DEFAULT_RECENT_DAYS: u32 = 30;

/// Where a view's items come from: files on disk (an event, a folder, a card) or developed
/// photographs (everything else).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ViewSource {
    /// The files of one event, across the folders and cards it spans.
    Event { event_id: EventId },
    /// The files of one folder on disk (On disk), with its subfolders when asked.
    Folder {
        path: PathBuf,
        #[serde(default)]
        subfolders: bool,
    },
    /// The files of one mounted camera card.
    Card { volume_id: VolumeId },
    /// Every photograph in the catalog but the removed.
    AllPhotographs,
    /// The photographs developed in the last `days` days (default 30).
    RecentlyDeveloped {
        #[serde(default = "default_recent_days")]
        days: u32,
    },
    /// The photographs of one catalog folder, and of its subfolders unless `subfolders` is false.
    CatalogFolder {
        folder_id: CatalogFolderId,
        #[serde(default = "yes")]
        subfolders: bool,
    },
    /// The members of a collection, or what a smart collection's stored query finds.
    Collection { collection_id: CollectionId },
    /// The photographs whose original is not where it was.
    MissingOriginals,
    /// The removed photographs, which no other source shows.
    Removed,
}

fn default_recent_days() -> u32 {
    DEFAULT_RECENT_DAYS
}

fn yes() -> bool {
    true
}

impl ViewSource {
    /// Whether the view lists files on disk rather than developed photographs.
    pub fn over_files(&self) -> bool {
        matches!(
            self,
            Self::Event { .. } | Self::Folder { .. } | Self::Card { .. }
        )
    }
}

/// An inclusive range of camera-local days.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DateRange {
    pub from: LocalDay,
    pub to: LocalDay,
}

/// What narrows a view. Every condition is optional; conditions combine with AND, the values of one
/// list condition with OR. `picked` and `without_pick` apply to files, `edited` to photographs; the
/// rest to both.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewFilter {
    /// Matches file names, places, cameras and lenses, ignoring case.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Picked files only (`true`), or files not picked (`false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub picked: Option<bool>,
    /// Moments without a pick: frames of bursts and brackets none of whose frames is picked, and
    /// unpicked singles.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub without_pick: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cameras: Vec<BodyKey>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lenses: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub kinds: Vec<SourceTag>,
    /// Photographs with history beyond their Original (`true`), or without (`false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dates: Option<DateRange>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<String>,
}

/// What a view is sorted by. Every sort ends in the same tie-breaks, so an order is total and
/// repeatable: equal keys fall back to capture time (earliest first), then the file name ignoring
/// case, then the path for files and the photograph's row for photographs. Undated items sort after
/// dated ones in either direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SortKey {
    /// [`CaptureTime::instant_ms`](super::CaptureTime::instant_ms); the only sort a view groups
    /// under.
    #[default]
    CaptureTime,
    /// When the photograph was developed; photographs only.
    DateDeveloped,
    /// The file's name, ignoring case.
    FileName,
    /// The photograph's newest history entry, or when it was developed if it has none since;
    /// photographs only.
    LastEdited,
}

/// A view's order. The default is capture time, earliest first, as a shoot is browsed; the desktop
/// asks for newest first over the catalog.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewSort {
    pub key: SortKey,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub descending: bool,
}

/// Everything that decides a view: the source, the filter, the sort, the grouping and its
/// thresholds. What `browse.view` evaluates and what a smart collection stores.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewQuery {
    pub source: ViewSource,
    #[serde(default)]
    pub filter: ViewFilter,
    #[serde(default)]
    pub sort: ViewSort,
    #[serde(default)]
    pub grouping: Grouping,
    #[serde(default)]
    pub thresholds: Thresholds,
}

impl ViewQuery {
    /// A query of `source` with every other part at its default.
    pub fn of(source: ViewSource) -> Self {
        Self {
            source,
            filter: ViewFilter::default(),
            sort: ViewSort::default(),
            grouping: Grouping::default(),
            thresholds: Thresholds::default(),
        }
    }

    /// Refuse a query no view can answer: a condition or sort that does not apply to its source's
    /// items, text over [`MAX_FILTER_TEXT`], an empty or reversed date range, or thresholds out of
    /// range. Whether the event, folder, card or collection it names exists is the evaluation's to
    /// check.
    pub fn validate(&self) -> Result<(), Error> {
        let files = self.source.over_files();
        let filter = &self.filter;
        if files && filter.edited.is_some() {
            return Err(Error::validation(
                "edited applies to photographs, not files",
            ));
        }
        if !files && (filter.picked.is_some() || filter.without_pick) {
            return Err(Error::validation(
                "picked and without_pick apply to files, not photographs",
            ));
        }
        if files && matches!(self.sort.key, SortKey::DateDeveloped | SortKey::LastEdited) {
            return Err(Error::validation(
                "date-developed and last-edited sort photographs, not files",
            ));
        }
        if filter
            .text
            .as_ref()
            .is_some_and(|text| text.chars().count() > MAX_FILTER_TEXT)
        {
            return Err(Error::validation(format!(
                "filter text is longer than {MAX_FILTER_TEXT} characters"
            )));
        }
        if filter.dates.is_some_and(|dates| dates.from > dates.to) {
            return Err(Error::validation("a date range ends before it starts"));
        }
        self.thresholds.validate()
    }

    /// The grouping the view is laid out with: the query's own under a capture-time sort, none
    /// under any other.
    pub fn effective_grouping(&self) -> Grouping {
        if self.sort.key == SortKey::CaptureTime {
            self.grouping
        } else {
            Grouping::None
        }
    }
}

/// What `browse.view` answers: the evaluated view's size, what it holds and its group layout,
/// without a row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewSummary {
    /// This client's view revision: it changes whenever the view is evaluated again, and every
    /// window, selection and position names the revision it belongs to.
    pub revision: u64,
    /// The query as evaluated, with every default filled in.
    pub query: ViewQuery,
    pub count: u32,
    /// Picked files in the view; 0 over photographs.
    pub picked: u32,
    /// Files in the view that are already a developed photograph's original; over photographs,
    /// the count.
    pub in_catalog: u32,
    /// Items whose file is offline or missing.
    pub unavailable: u32,
    pub groups: GroupLayout,
    /// The library journal and the index revision the view was evaluated at: a later library change
    /// or index change marks it stale ([`BrowseSession::stale`]).
    pub library_sequence: LibraryChangeSeq,
    pub index_revision: u64,
}

/// A row's identity: a file of the index, named also by its path, or a developed photograph.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "item", rename_all = "kebab-case")]
pub enum RowItem {
    File { file_id: FileId },
    Photo { asset_id: AssetId },
}

/// Where a row sits in its moment: the moment's index in the view summary's `groups.moments` and
/// the frame's place in it, from 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MomentRef {
    pub index: u32,
    pub frame: u32,
}

/// What the grid can draw for a row now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PreviewState {
    /// Nothing cached yet; the preview lane will read it.
    Pending,
    /// Only the file's tiny EXIF thumbnail is ready: the cell is drawn soft.
    Thumbnail,
    /// The grid tier is cached.
    Ready,
    /// Nothing can be drawn: the file is unreadable or carries no preview, and no fallback exists.
    Unavailable,
}

/// One row of a view, as `browse.rows` answers it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ViewRow {
    pub position: u32,
    #[serde(flatten)]
    pub item: RowItem,
    /// The file's path, or the photograph's original's.
    pub path: PathBuf,
    pub file_name: String,
    pub kind: SourceTag,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dimensions: Option<Dimensions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<ExifOrientation>,
    /// [`CaptureTime::text`](super::CaptureTime::text); absent when undated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub place: Option<String>,
    /// The body's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lens: Option<String>,
    #[serde(default)]
    pub exposure: Exposure,
    /// Absent for a single.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moment: Option<MomentRef>,
    /// A file's pick; false for a photograph.
    pub picked: bool,
    /// A file that is already a developed photograph's original names that photograph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub developed_as: Option<AssetId>,
    /// A photograph with history beyond its Original; false for a file.
    pub edited: bool,
    pub availability: FileAvailability,
    pub preview: PreviewState,
}

/// What `browse.rows` answers: rows `from..from + rows.len()` of the view at `revision`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewRows {
    pub revision: u64,
    pub from: u32,
    pub rows: Vec<ViewRow>,
}

/// A metadata-browser column: what `browse.facets` counts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Facet {
    /// Camera-local days (`YYYY-MM-DD`); a client folds them into months and years.
    Date,
    Place,
    /// Body keys, labelled.
    Camera,
    Lens,
    /// `jpeg` or `raw`.
    Kind,
    /// `picked` or `not-picked`; files only.
    Pick,
}

impl Facet {
    pub const ALL: [Self; 6] = [
        Self::Date,
        Self::Place,
        Self::Camera,
        Self::Lens,
        Self::Kind,
        Self::Pick,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Place => "place",
            Self::Camera => "camera",
            Self::Lens => "lens",
            Self::Kind => "kind",
            Self::Pick => "pick",
        }
    }
}

/// One value of a facet and how many items have it. `value` is absent for the items that record
/// none (undated, no place, no lens); `label` is what a person reads when it differs from the value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FacetValue {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub count: u32,
}

/// What `browse.facets` answers: for each facet asked, every value with its count, most frequent
/// first. A count is exactly the size of the view the query gives with that facet's own condition
/// replaced by that one value, so a count always equals the view it predicts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facets {
    pub counts: BTreeMap<Facet, Vec<FacetValue>>,
}

/// How `browse.select` changes the selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionMode {
    #[default]
    Replace,
    Add,
    Remove,
    Toggle,
}

impl SelectionMode {
    pub const ALL: [Self; 4] = [Self::Replace, Self::Add, Self::Remove, Self::Toggle];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Replace => "replace",
            Self::Add => "add",
            Self::Remove => "remove",
            Self::Toggle => "toggle",
        }
    }
}

/// Positions `start..start + len` of a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PositionRange {
    pub start: u32,
    pub len: u32,
}

/// The selection in a client's view, as `session.state` reports it: disjoint, ascending ranges of
/// positions in the view at the session's `browse.revision`, their total, and the active item. When
/// the view is evaluated again the owner carries the selection over by item, so positions follow
/// the items they named.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ViewSelection {
    pub count: u32,
    pub ranges: Vec<PositionRange>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active: Option<u32>,
}

/// A client's view as its session carries it (`session.state`'s `browse`): the query it was
/// evaluated from, its revision and size, whether a later library or index change made it stale,
/// and the selection. The item list itself stays with the owner.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BrowseSession {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<ViewQuery>,
    pub revision: u64,
    pub count: u32,
    pub stale: bool,
    pub selection: ViewSelection,
    /// The library journal and index revision the view was evaluated at, which every read of the
    /// session compares with the current ones to set `stale`; none without a view. Never
    /// serialized: the view's summary reports them.
    #[serde(skip)]
    pub evaluated_at: Option<ViewStamp>,
}

/// What a view was evaluated at: the catalog's latest library change and the index's revision.
/// A view is stale once either has moved on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ViewStamp {
    pub library_sequence: LibraryChangeSeq,
    pub index_revision: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_query_names_only_its_source_and_defaults_the_rest() {
        let event = EventId::of(std::path::Path::new("/a/b.jpg"), 0);
        let query: ViewQuery = serde_json::from_value(json!({
            "source": {"kind": "event", "event_id": event},
        }))
        .unwrap();
        assert_eq!(query, ViewQuery::of(ViewSource::Event { event_id: event }));
        query.validate().unwrap();
        assert_eq!(
            serde_json::to_value(query.sort).unwrap(),
            json!({"key": "capture-time"})
        );
        let recent: ViewSource =
            serde_json::from_value(json!({"kind": "recently-developed"})).unwrap();
        assert_eq!(recent, ViewSource::RecentlyDeveloped { days: 30 });
        let all: ViewSource = serde_json::from_value(json!({"kind": "all-photographs"})).unwrap();
        assert!(!all.over_files());
        assert!(
            serde_json::from_value::<ViewSource>(
                json!({"kind": "card", "volume_id": "volume-0123456789", "x": 1})
            )
            .is_err()
        );
    }

    #[test]
    fn a_query_refuses_conditions_its_items_do_not_have() {
        let mut files = ViewQuery::of(ViewSource::Folder {
            path: "/p".into(),
            subfolders: false,
        });
        files.filter.edited = Some(true);
        assert!(files.validate().is_err());
        let mut photos = ViewQuery::of(ViewSource::AllPhotographs);
        photos.filter.picked = Some(true);
        assert!(photos.validate().is_err());
        photos.filter.picked = None;
        photos.sort.key = SortKey::LastEdited;
        photos.validate().unwrap();
        assert_eq!(photos.effective_grouping(), Grouping::None);
        photos.filter.dates = Some(DateRange {
            from: LocalDay(10),
            to: LocalDay(9),
        });
        assert!(photos.validate().is_err());
    }

    #[test]
    fn a_row_names_a_file_with_its_path_and_a_photograph_by_its_asset() {
        let row = ViewRow {
            position: 4,
            item: RowItem::File { file_id: FileId(9) },
            path: "/card/DSC_0001.NEF".into(),
            file_name: "DSC_0001.NEF".into(),
            kind: SourceTag::Raw,
            dimensions: None,
            orientation: None,
            capture: None,
            place: None,
            camera: None,
            lens: None,
            exposure: Exposure::default(),
            moment: None,
            picked: true,
            developed_as: None,
            edited: false,
            availability: FileAvailability::Available,
            preview: PreviewState::Thumbnail,
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(json["item"], "file");
        assert_eq!(json["file_id"], 9);
        assert_eq!(json["path"], "/card/DSC_0001.NEF");
        assert_eq!(serde_json::from_value::<ViewRow>(json).unwrap(), row);
        let facets = Facets {
            counts: BTreeMap::from([(
                Facet::Camera,
                vec![FacetValue {
                    value: Some("FUJIFILM|X100VI|".into()),
                    label: Some("FUJIFILM X100VI".into()),
                    count: 3,
                }],
            )]),
        };
        assert_eq!(
            serde_json::to_value(&facets).unwrap()["counts"]["camera"][0]["count"],
            3
        );
    }
}
