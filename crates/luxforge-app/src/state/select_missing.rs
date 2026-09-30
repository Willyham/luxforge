//! Missing originals in the Select workspace ([catalog design](../../../../docs/design/catalog.md#missing-originals),
//! [resolve board](../../../../docs/design/catalog/resolve-missing.png)): the photographs whose
//! originals are not where they were, grouped by the folder on disk each was developed from, what a
//! search of a chosen folder found for each, and the Relink that commits exactly what was verified.
//! Locate original… in Develop's Original not found notice shares its Locate. **Lane D (views and
//! desktop)** owns it. Like every view model it names no framework type, no widget and no view.
//!
//! The desktop holds no catalog logic. The groups and their reasons are `source.missing`'s answer
//! and each row's result is its search's `source.find` report, read with `job.read`: nothing here
//! decides whether a file is a photograph's original. Every gesture is the request an API client
//! would send ([`find_params`], [`relink_params`], [`locate_params`], [`cancel_params`]), and
//! Relink sends exactly the pairs a finished search verified and the choices made among several
//! identical files ([`relink_pairs`]). Nothing changes until Relink or a Locate: a search changes
//! nothing, and a stopped or failed one leaves its group as it was.
use super::select::{SelectState, Shown, photographs, shown_path, thousands};
use luxforge_core::{
    AssetId, MutationRequest,
    catalog_types::{
        FindResult, FindRow, MissingGroup, MissingOriginals, MissingReason, RelinkPair, ViewSource,
        VolumeState,
    },
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[cfg(test)]
pub(crate) mod tests;

/// The filter segments over the rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum MissingFilter {
    #[default]
    All,
    /// Found with the original's bytes, or chosen among several identical files.
    Found,
    /// A decision only a person can make: bytes that differ, several identical files not chosen
    /// between yet, or a file another photograph names.
    NeedsYou,
    NotFound,
}

impl MissingFilter {
    pub(crate) const ALL: [Self; 4] = [Self::All, Self::Found, Self::NeedsYou, Self::NotFound];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Found => "Found",
            Self::NeedsYou => "Needs you",
            Self::NotFound => "Not found",
        }
    }

    /// Whether a row of this result is shown under the filter. A photograph still being checked is
    /// shown under All alone.
    fn admits(self, kind: Kind) -> bool {
        match self {
            Self::All => true,
            Self::Found => kind == Kind::Found,
            Self::NeedsYou => kind == Kind::NeedsYou,
            Self::NotFound => kind == Kind::NotFound,
        }
    }
}

/// Which filter a row falls under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Found,
    NeedsYou,
    NotFound,
    Checking,
}

/// Where one group's search stands.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SearchStatus {
    /// `source.find` is on its way; the job is not known yet.
    Starting,
    /// The job runs: its report so far is on screen.
    Running { job: String },
    /// Stop search was pressed: `job.cancel` is sent and the job's end awaited.
    Stopping { job: String },
    /// The search finished: its report is final, and the owner remembers what it verified.
    Ended,
    /// The search failed (a folder it could not read, a drive disconnected): why. It remembers
    /// nothing.
    Failed(String),
}

impl SearchStatus {
    /// The search's job, while it has one that has not ended.
    pub(crate) fn job(&self) -> Option<&str> {
        match self {
            Self::Running { job } | Self::Stopping { job } => Some(job),
            _ => None,
        }
    }

    /// The search has not ended.
    pub(crate) fn live(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running { .. } | Self::Stopping { .. }
        )
    }
}

/// One group's search of a chosen folder: its report, so far or final, and the choices made among
/// several identical files.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Search {
    /// The folder searched, as the dialog chose it.
    pub(crate) root: PathBuf,
    pub(crate) status: SearchStatus,
    /// The job's progress message while it runs.
    pub(crate) progress: Option<String>,
    pub(crate) rows: Vec<FindRow>,
    /// The file chosen for a photograph whose result is several identical files.
    pub(crate) chosen: BTreeMap<AssetId, PathBuf>,
}

impl Search {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            status: SearchStatus::Starting,
            progress: None,
            rows: Vec::new(),
            chosen: BTreeMap::new(),
        }
    }
}

/// Where a Locate was asked from, which says what follows its success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LocateFrom {
    /// A row's Locate…, or the Info panel's Locate a different file…: the list is read again.
    Missing,
    /// Develop's Original not found notice: the photograph is opened again from its new file.
    Develop,
}

/// A photograph a Locate is pointing at a chosen file.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Locating {
    pub(crate) asset_id: AssetId,
    /// Its original's file name, which the status bar names.
    pub(crate) file_name: String,
    pub(crate) path: PathBuf,
    pub(crate) from: LocateFrom,
    /// The `source-locate` job, once the owner has answered with it.
    pub(crate) job: Option<String>,
}

/// What the Info panel says of the selected photograph beyond its row: where its original was
/// (`asset.state`'s locator) and how many history entries it has (`history.list`'s newest
/// sequence, from the Original's 0).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PhotoFacts {
    pub(crate) was: PathBuf,
    pub(crate) entries: u64,
}

/// What Missing originals holds that its model reads: what the owner last answered, the searches,
/// the local choices and what is in flight.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MissingState {
    /// `source.missing`'s last answer.
    pub(crate) list: Option<MissingOriginals>,
    /// `volume.list`'s, read with it for the volumes' labels.
    pub(crate) volumes: Vec<VolumeState>,
    /// Why the last read failed, until one succeeds.
    pub(crate) error: Option<String>,
    /// The Select shell's evaluation the list was last asked for: the view is read once per
    /// evaluation of the Missing originals source, and again after a relink or a Locate.
    pub(crate) read_for: Option<u64>,
    /// A read is in flight.
    pub(crate) reading: bool,
    /// Each group's search, by its folder on disk. At most one has not ended.
    pub(crate) searches: BTreeMap<PathBuf, Search>,
    pub(crate) filter: MissingFilter,
    /// The row the Info panel describes: this desktop's own view state.
    pub(crate) selected: Option<AssetId>,
    /// The selected photograph's facts, as the owner answered them.
    pub(crate) facts: Option<(AssetId, Result<PhotoFacts, String>)>,
    /// The photograph whose facts are being read.
    pub(crate) facts_reading: Option<AssetId>,
    /// The row whose Choose… menu is open.
    pub(crate) menu: Option<AssetId>,
    pub(crate) locating: Option<Locating>,
    /// `source.relink` is in flight.
    pub(crate) relinking: bool,
    /// A `job.read` of the running jobs is in flight.
    pub(crate) polling: bool,
}

impl MissingState {
    /// The one search that has not ended, with its group's folder.
    pub(crate) fn live_search(&self) -> Option<(&PathBuf, &Search)> {
        self.searches
            .iter()
            .find(|(_, search)| search.status.live())
    }

    /// The jobs to read while they run: the live search's and the Locate's.
    pub(crate) fn running_jobs(&self) -> (Option<String>, Option<String>) {
        let search = self
            .live_search()
            .and_then(|(_, search)| search.status.job().map(str::to_owned));
        let locate = self
            .locating
            .as_ref()
            .and_then(|locating| locating.job.clone());
        (search, locate)
    }

    /// The row of `asset`, with the search it belongs to.
    pub(crate) fn row(&self, asset: &AssetId) -> Option<(&PathBuf, &Search, &FindRow)> {
        self.searches.iter().find_map(|(folder, search)| {
            search
                .rows
                .iter()
                .find(|row| &row.asset_id == asset)
                .map(|row| (folder, search, row))
        })
    }

    /// Forget the rows of photographs that no longer miss their originals: relinked or located.
    pub(crate) fn resolved(&mut self, assets: &[AssetId]) {
        for search in self.searches.values_mut() {
            search.rows.retain(|row| !assets.contains(&row.asset_id));
            search.chosen.retain(|asset, _| !assets.contains(asset));
        }
        self.searches
            .retain(|_, search| search.status.live() || !search.rows.is_empty());
        if self
            .selected
            .as_ref()
            .is_some_and(|selected| assets.contains(selected))
        {
            self.selected = None;
        }
        if self.menu.as_ref().is_some_and(|menu| assets.contains(menu)) {
            self.menu = None;
        }
    }

    /// Adopt a new list: a group that is no longer missing anything drops its search, and a
    /// selection or a menu whose row went with it is closed.
    pub(crate) fn listed(&mut self, list: MissingOriginals, volumes: Vec<VolumeState>) {
        self.searches.retain(|folder, search| {
            search.status.live()
                || list
                    .groups
                    .iter()
                    .any(|group| &group.source_folder == folder)
        });
        let known = |asset: &AssetId| {
            self.searches
                .values()
                .any(|search| search.rows.iter().any(|row| &row.asset_id == asset))
        };
        if self.selected.as_ref().is_some_and(|asset| !known(asset)) {
            self.selected = None;
        }
        if self.menu.as_ref().is_some_and(|asset| !known(asset)) {
            self.menu = None;
        }
        self.list = Some(list);
        self.volumes = volumes;
        self.error = None;
    }

    /// Nothing Missing originals asked the owner for is in flight or still wanted: the list for
    /// the evaluation on screen, a search, a Locate, a relink and the selected row's facts.
    pub(crate) fn quiet(&self, shown: bool, serial: u64) -> bool {
        !self.reading
            && !self.relinking
            && !self.polling
            && self.live_search().is_none()
            && self.locating.is_none()
            && self.facts_reading.is_none()
            && (!shown
                || (self.read_for == Some(serial)
                    && self.selected.as_ref().is_none_or(|asset| {
                        self.facts.as_ref().is_some_and(|(held, _)| held == asset)
                    })))
    }
}

/// Whether Select shows Missing originals.
pub(crate) fn shown(state: &SelectState) -> bool {
    state.shown == Shown::Select
        && state
            .query
            .as_ref()
            .is_some_and(|query| query.source == ViewSource::MissingOriginals)
}

// -- The requests, each exactly what an API client sends. --

/// `source.find` for every missing photograph developed from `source_folder`, searching `root` and
/// its subfolders.
pub(crate) fn find_params(root: &Path, source_folder: &Path) -> Value {
    json!({"search_root": root, "source_folder": source_folder})
}

/// `job.read` and `job.cancel` of one job.
pub(crate) fn job_params(job: &str) -> Value {
    json!({ "job_id": job })
}

/// `job.cancel` of a search: Stop search.
pub(crate) fn cancel_params(job: &str) -> Value {
    job_params(job)
}

/// `source.relink` of `pairs` as one library change.
pub(crate) fn relink_params(pairs: &[RelinkPair], mutation: &MutationRequest) -> Value {
    json!({"pairs": pairs, "mutation": mutation})
}

/// `source.locate` of one photograph and the file chosen for it.
pub(crate) fn locate_params(asset: &AssetId, path: &Path, mutation: &MutationRequest) -> Value {
    json!({"asset_id": asset, "path": path, "mutation": mutation})
}

/// `history.list`'s newest row for a photograph, whose sequence counts its entries from the
/// Original's 0.
pub(crate) fn newest_entry_params(asset: &AssetId) -> Value {
    json!({"asset_id": asset, "before_sequence": null, "limit": 1})
}

/// What Relink commits: every pair a finished search verified — each `found` file, and the file
/// chosen for a photograph with several identical files — in the order the groups and their rows
/// are listed. A search that has not finished verified nothing the owner remembers yet, and a
/// stopped or failed one nothing at all, so neither contributes.
pub(crate) fn relink_pairs(state: &MissingState) -> Vec<RelinkPair> {
    verified(state)
        .map(|(asset_id, path)| RelinkPair {
            asset_id: asset_id.clone(),
            path: path.clone(),
        })
        .collect()
}

/// Each photograph Relink would point at a file, and the file, without copying either.
fn verified(state: &MissingState) -> impl Iterator<Item = (&AssetId, &PathBuf)> {
    state
        .searches
        .values()
        .filter(|search| search.status == SearchStatus::Ended)
        .flat_map(|search| {
            search.rows.iter().filter_map(move |row| {
                let path = match &row.result {
                    FindResult::Found { path } => Some(path),
                    FindResult::SeveralIdentical { paths } => search
                        .chosen
                        .get(&row.asset_id)
                        .filter(|chosen| paths.contains(chosen)),
                    _ => None,
                };
                path.map(|path| (&row.asset_id, path))
            })
        })
}

// -- The model. --

/// The glyph and ink a row's result is drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResultGlyph {
    /// Found, or chosen: the check in the connected green.
    Found,
    /// Bytes that differ, or a file another photograph names: the warning in the clipping red.
    Refused,
    /// Several identical files: the stack.
    Several,
    /// Not found: the search glyph.
    NotFound,
    /// Still checking: the clock.
    Checking,
}

/// A row's action at its right.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RowAction {
    /// Locate…, disabled while a Locate is running.
    Locate { enabled: bool },
    /// Choose… with its menu of the identical files, open or not, each labelled under the search
    /// root and checked when chosen.
    Choose {
        open: bool,
        choices: Vec<(String, PathBuf, bool)>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RowModel {
    pub(crate) asset_id: AssetId,
    pub(crate) name: String,
    pub(crate) folder: Option<String>,
    pub(crate) glyph: ResultGlyph,
    pub(crate) text: String,
    pub(crate) detail: Option<String>,
    pub(crate) action: Option<RowAction>,
    pub(crate) selected: bool,
}

/// A header's or the Info panel's action: its label, and why it is refused when it is.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ActionModel {
    pub(crate) label: String,
    pub(crate) reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupModel {
    /// The folder on disk, which Find in a folder… names.
    pub(crate) folder: PathBuf,
    pub(crate) path: String,
    pub(crate) detail: String,
    pub(crate) status: Option<String>,
    pub(crate) find: Option<ActionModel>,
    /// Its rows under the filter, within [`MAX_DRAWN_ROWS`] across the groups.
    pub(crate) rows: Vec<RowModel>,
    /// What the rows leave out past [`MAX_DRAWN_ROWS`]: "1,200 more: narrow them with the filters".
    pub(crate) more: Option<String>,
}

/// The most rows the groups draw together. A search may answer for tens of thousands of
/// photographs, and the view draws every row it is given, so the groups draw their first rows, in
/// order, and each says how many more it has; Relink still sends every verified pair, and the
/// filters narrow what is drawn.
pub(crate) const MAX_DRAWN_ROWS: usize = 500;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct BarModel {
    pub(crate) verified: String,
    pub(crate) detail: Option<String>,
    pub(crate) stop: bool,
    pub(crate) relink: String,
    /// Relink's pairs; zero disables it.
    pub(crate) pairs: usize,
    /// Why Relink is refused.
    pub(crate) reason: Option<String>,
}

/// The Info panel for the selected row.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PhotoInfo {
    pub(crate) asset_id: AssetId,
    pub(crate) name: String,
    pub(crate) rows: Vec<(String, String)>,
    /// Whether Check's dot is the verified green.
    pub(crate) verified: bool,
    pub(crate) locate: ActionModel,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum MissingInfo {
    #[default]
    Nothing,
    One(PhotoInfo),
}

/// What Missing originals shows, derived after every message while Select shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MissingModel {
    pub(crate) shown: bool,
    /// "252 photographs whose originals are not where they were" and its note.
    pub(crate) heading: Option<(String, String)>,
    /// Said in the centre instead of the groups: reading, a failure, nothing missing.
    pub(crate) note: Option<String>,
    /// The filter segments: each label with its count, and the one selected.
    pub(crate) filters: Vec<(String, Option<String>)>,
    pub(crate) filter: usize,
    pub(crate) groups: Vec<GroupModel>,
    pub(crate) bar: Option<BarModel>,
    pub(crate) info: MissingInfo,
    /// The title bar's summary: "252 in the catalog · Photos SSD is not connected".
    pub(crate) title: String,
    /// The status bar's line: "252 missing · 160 found".
    pub(crate) line: String,
}

/// The note under the Info panel's action.
pub(crate) const RELINK_NOTE: &str = "Relinking changes only where the catalog looks for this \
    file. Its catalog folder, edits and history stay.";

/// Select's model with Missing originals' own title summary and status line while it is shown:
/// what is missing and why, rather than the view's count of developed photographs.
pub(crate) fn over(mut model: super::select::SelectModel) -> super::select::SelectModel {
    if model.missing.shown {
        model.title.summary = model.missing.title.clone();
        model.status.line = model.missing.line.clone();
    }
    model
}

/// The model of Missing originals while Select shows it; empty otherwise.
pub(crate) fn derive(state: &SelectState) -> MissingModel {
    if !shown(state) {
        return MissingModel::default();
    }
    let missing = &state.missing;
    let home = state.home.as_deref();
    let counts = Counts::of(missing);
    let filter_index = MissingFilter::ALL
        .iter()
        .position(|filter| *filter == missing.filter)
        .unwrap_or(0);
    let count_label = |count: usize| (count > 0).then(|| thousands(count as u32));
    let filters = MissingFilter::ALL
        .iter()
        .map(|filter| {
            let count = match filter {
                MissingFilter::All => None,
                MissingFilter::Found => count_label(counts.found),
                MissingFilter::NeedsYou => count_label(counts.needs_you()),
                MissingFilter::NotFound => count_label(counts.not_found),
            };
            (filter.label().to_owned(), count)
        })
        .collect();
    let Some(list) = &missing.list else {
        let note = match &missing.error {
            Some(error) => format!("Missing originals unavailable: {error}"),
            None => "Reading\u{2026}".to_owned(),
        };
        return MissingModel {
            shown: true,
            note: Some(note),
            filters,
            filter: filter_index,
            title: String::new(),
            line: String::new(),
            ..MissingModel::default()
        };
    };
    let live = missing.live_search().map(|(folder, _)| folder.clone());
    let mut budget = MAX_DRAWN_ROWS;
    let groups: Vec<GroupModel> = list
        .groups
        .iter()
        .map(|group| group_model(missing, group, live.as_ref(), home, &mut budget))
        .filter(|group| {
            missing.filter == MissingFilter::All || !group.rows.is_empty() || group.more.is_some()
        })
        .collect();
    let heading = (list.count > 0).then(|| {
        (
            if list.count == 1 {
                "1 photograph whose original is not where it was".to_owned()
            } else {
                format!(
                    "{} photographs whose originals are not where they were",
                    thousands(list.count)
                )
            },
            "grouped by the folder on disk each was developed from".to_owned(),
        )
    });
    let note = if list.count == 0 {
        Some("Every original is where it was".to_owned())
    } else if groups.is_empty() {
        Some(format!(
            "No photograph is {}",
            match missing.filter {
                MissingFilter::Found => "found yet",
                MissingFilter::NeedsYou => "waiting for you",
                MissingFilter::NotFound => "not found",
                MissingFilter::All => "missing",
            }
        ))
    } else {
        None
    };
    let mut title = vec![format!("{} in the catalog", thousands(list.count))];
    let mut offline: Vec<&str> = Vec::new();
    for group in &list.groups {
        if let MissingReason::VolumeOffline { label } = &group.reason
            && !offline.contains(&label.as_str())
        {
            offline.push(label);
        }
    }
    title.extend(
        offline
            .iter()
            .map(|label| format!("{label} is not connected")),
    );
    MissingModel {
        shown: true,
        heading,
        note,
        filters,
        filter: filter_index,
        groups,
        bar: bar(missing, &counts),
        info: info(missing, list, home),
        title: title.join(" \u{b7} "),
        line: format!(
            "{} missing \u{b7} {} found",
            thousands(list.count),
            thousands(counts.found as u32)
        ),
    }
}

/// How many rows fall under each result, across every search.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    found: usize,
    different: usize,
    choose: usize,
    claimed: usize,
    not_found: usize,
    checking: usize,
}

impl Counts {
    fn of(state: &MissingState) -> Self {
        let mut counts = Self::default();
        for search in state.searches.values() {
            for row in &search.rows {
                match (&row.result, search.chosen.contains_key(&row.asset_id)) {
                    (FindResult::Found { .. }, _) | (FindResult::SeveralIdentical { .. }, true) => {
                        counts.found += 1
                    }
                    (FindResult::SeveralIdentical { .. }, false) => counts.choose += 1,
                    (FindResult::DifferentBytes { .. }, _) => counts.different += 1,
                    (FindResult::Claimed { .. }, _) => counts.claimed += 1,
                    (FindResult::NotFound, _) => counts.not_found += 1,
                    (FindResult::Checking, _) => counts.checking += 1,
                }
            }
        }
        counts
    }

    fn needs_you(self) -> usize {
        self.different + self.choose + self.claimed
    }

    fn any(self) -> bool {
        self.found + self.needs_you() + self.not_found + self.checking > 0
    }
}

fn kind(result: &FindResult, chosen: bool) -> Kind {
    match result {
        FindResult::Found { .. } => Kind::Found,
        FindResult::SeveralIdentical { .. } if chosen => Kind::Found,
        FindResult::SeveralIdentical { .. }
        | FindResult::DifferentBytes { .. }
        | FindResult::Claimed { .. } => Kind::NeedsYou,
        FindResult::NotFound => Kind::NotFound,
        FindResult::Checking => Kind::Checking,
    }
}

/// `A`, `A and B`, `A, B and C`, `A, B and 3 others`.
fn folders_text(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [one, two, three] => format!("{one}, {two} and {three}"),
        [one, two, rest @ ..] => format!("{one}, {two} and {} others", rest.len()),
    }
}

/// Why a group is missing, in a sentence: "Photos SSD is not connected", "the folder is gone from
/// Macintosh HD". A folder's volume is named from `volume.list` when it knows it.
pub(crate) fn reason_text(group: &MissingGroup, volumes: &[VolumeState]) -> String {
    let label = volumes
        .iter()
        .find(|state| state.volume.id == group.volume_id)
        .map(|state| state.volume.label.as_str())
        .filter(|label| !label.is_empty());
    match (&group.reason, label) {
        (MissingReason::VolumeOffline { label }, _) => format!("{label} is not connected"),
        (MissingReason::FolderGone, Some(label)) => format!("the folder is gone from {label}"),
        (MissingReason::FolderGone, None) => "the folder is gone".to_owned(),
        (MissingReason::FilesGone, _) => "the files are gone from the folder".to_owned(),
        (MissingReason::Changed, _) => "other files are at their paths now".to_owned(),
    }
}

/// `path` under `root`, as a row names where a file is: `…/Photographs/2026-08 Lake/` for the
/// folder that holds it, or the whole path when it is not under the root.
fn under_root(root: &Path, path: &Path, home: Option<&Path>) -> String {
    match path
        .parent()
        .and_then(|parent| parent.strip_prefix(root).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "\u{2026}/".to_owned(),
        Some(rest) => format!("\u{2026}/{}/", rest.display()),
        None => shown_path(path, home),
    }
}

/// A file under `root` as the Choose… menu lists it: its path from the root.
fn choice_label(root: &Path, path: &Path, home: Option<&Path>) -> String {
    match path.strip_prefix(root) {
        Ok(rest) => format!("\u{2026}/{}", rest.display()),
        Err(_) => shown_path(path, home),
    }
}

/// The characters a path takes before its middle gives way: in a group's header, in a status and
/// in the Info panel, whose value column is narrow.
const HEADER_PATH: usize = 64;
const STATUS_PATH: usize = 48;
const INFO_PATH: usize = 36;

/// `path` as the view shows it (under `~` for the home folder), and, past `most` characters, its
/// first component and as many of its last as fit, with `…` between: the folder a path ends in is
/// what tells two groups apart, and an ellipsis at the end would drop it.
pub(crate) fn short_path(path: &Path, home: Option<&Path>, most: usize) -> String {
    let shown = shown_path(path, home);
    if shown.chars().count() <= most {
        return shown;
    }
    let separator = std::path::MAIN_SEPARATOR_STR;
    let parts: Vec<&str> = shown.split(['/', '\\']).collect();
    // An absolute path's first part is empty: its head is the root and its first folder.
    let head_len = if parts.first().is_some_and(|part| part.is_empty()) {
        2
    } else {
        1
    };
    if parts.len() <= head_len + 1 {
        return shown;
    }
    let head = parts[..head_len].join(separator);
    let mut used = head.chars().count() + 2;
    let mut tail = Vec::new();
    for part in parts[head_len..].iter().rev() {
        let len = part.chars().count() + 1;
        if !tail.is_empty() && used + len > most {
            break;
        }
        used += len;
        tail.push(*part);
    }
    if tail.len() == parts.len() - head_len {
        return shown;
    }
    tail.reverse();
    format!(
        "{head}{separator}\u{2026}{separator}{}",
        tail.join(separator)
    )
}

fn group_model(
    state: &MissingState,
    group: &MissingGroup,
    live: Option<&PathBuf>,
    home: Option<&Path>,
    budget: &mut usize,
) -> GroupModel {
    let search = state.searches.get(&group.source_folder);
    let names: Vec<String> = group
        .catalog_folders
        .iter()
        .map(|folder| folder.name.clone())
        .collect();
    let reason = reason_text(group, &state.volumes);
    let offline = matches!(group.reason, MissingReason::VolumeOffline { .. });
    let mut detail = vec![photographs(group.count)];
    if !names.is_empty() {
        detail.push(format!("in {}", folders_text(&names)));
    }
    detail.push(if offline && search.is_none() {
        format!("{reason}: connect it, or find the files elsewhere")
    } else {
        reason
    });
    let status = search.map(|search| {
        let root = short_path(&search.root, home, STATUS_PATH);
        match &search.status {
            SearchStatus::Starting => format!("Searching {root}\u{2026}"),
            SearchStatus::Running { .. } => match &search.progress {
                Some(progress) => format!("Searching {root} \u{b7} {progress}"),
                None => format!("Searching {root}\u{2026}"),
            },
            SearchStatus::Stopping { .. } => "Stopping the search\u{2026}".to_owned(),
            SearchStatus::Ended => format!("Searched {root}"),
            SearchStatus::Failed(reason) => format!("The search failed: {reason}"),
        }
    });
    let find = match live {
        Some(folder) if folder == &group.source_folder => None,
        Some(_) => Some(ActionModel {
            label: "Find in a folder\u{2026}".to_owned(),
            reason: Some("One search at a time: stop this one first".to_owned()),
        }),
        None => Some(ActionModel {
            label: if search.is_some() {
                "Find again\u{2026}".to_owned()
            } else {
                "Find in a folder\u{2026}".to_owned()
            },
            reason: None,
        }),
    };
    let folder = (names.len() == 1).then(|| names[0].clone());
    let mut rows = Vec::new();
    let mut admitted = 0_usize;
    if let Some(search) = search {
        for row in &search.rows {
            let chosen = search.chosen.contains_key(&row.asset_id);
            if !state.filter.admits(kind(&row.result, chosen)) {
                continue;
            }
            admitted += 1;
            if *budget > 0 {
                *budget -= 1;
                rows.push(row_model(state, search, row, folder.clone(), home));
            }
        }
    }
    let more = (admitted > rows.len()).then(|| {
        format!(
            "{} more: narrow them with the filters",
            thousands((admitted - rows.len()) as u32)
        )
    });
    GroupModel {
        folder: group.source_folder.clone(),
        path: short_path(&group.source_folder, home, HEADER_PATH),
        detail: detail.join(" \u{b7} "),
        status,
        find,
        rows,
        more,
    }
}

fn row_model(
    state: &MissingState,
    search: &Search,
    row: &FindRow,
    folder: Option<String>,
    home: Option<&Path>,
) -> RowModel {
    let root = &search.root;
    let locating = state
        .locating
        .as_ref()
        .is_some_and(|locating| locating.asset_id == row.asset_id);
    let locate = Some(RowAction::Locate {
        enabled: state.locating.is_none(),
    });
    let (glyph, text, detail, action) = match &row.result {
        FindResult::Found { path } => (
            ResultGlyph::Found,
            "Found, same bytes".to_owned(),
            Some(under_root(root, path, home)),
            None,
        ),
        FindResult::SeveralIdentical { paths } => {
            let chosen = search.chosen.get(&row.asset_id);
            let choices = paths
                .iter()
                .map(|path| {
                    (
                        choice_label(root, path, home),
                        path.clone(),
                        chosen == Some(path),
                    )
                })
                .collect();
            let action = Some(RowAction::Choose {
                open: state.menu.as_ref() == Some(&row.asset_id),
                choices,
            });
            match chosen {
                Some(path) => (
                    ResultGlyph::Found,
                    "Chosen, same bytes".to_owned(),
                    Some(under_root(root, path, home)),
                    action,
                ),
                None => (
                    ResultGlyph::Several,
                    format!("{} files with the same bytes", paths.len()),
                    Some("choose one".to_owned()),
                    action,
                ),
            }
        }
        FindResult::DifferentBytes { .. } => (
            ResultGlyph::Refused,
            "Different bytes at the same name".to_owned(),
            Some("left as it is".to_owned()),
            locate,
        ),
        FindResult::Claimed { .. } => (
            ResultGlyph::Refused,
            "Another photograph uses this file".to_owned(),
            Some("never taken".to_owned()),
            locate,
        ),
        FindResult::NotFound => (
            ResultGlyph::NotFound,
            format!("Not found under {}", short_path(root, home, STATUS_PATH)),
            None,
            locate,
        ),
        FindResult::Checking => (
            ResultGlyph::Checking,
            "Checking\u{2026}".to_owned(),
            None,
            None,
        ),
    };
    let (text, detail) = if locating {
        ("Locating\u{2026}".to_owned(), None)
    } else {
        (text, detail)
    };
    RowModel {
        asset_id: row.asset_id.clone(),
        name: row.file_name.clone(),
        folder,
        glyph: if locating {
            ResultGlyph::Checking
        } else {
            glyph
        },
        text,
        detail,
        action: if locating {
            Some(RowAction::Locate { enabled: false })
        } else {
            action
        },
        selected: state.selected.as_ref() == Some(&row.asset_id),
    }
}

fn bar(state: &MissingState, counts: &Counts) -> Option<BarModel> {
    let live = state.live_search();
    if !counts.any() && live.is_none() {
        return None;
    }
    let pairs = verified(state).count();
    let mut detail = Vec::new();
    let mut part = |count: usize, words: &str| {
        if count > 0 {
            detail.push(format!("{} {words}", thousands(count as u32)));
        }
    };
    part(counts.different, "different");
    part(counts.choose, "to choose");
    part(counts.claimed, "claimed");
    part(counts.not_found, "not found");
    part(counts.checking, "checking");
    let reason = if state.relinking {
        Some("Relinking\u{2026}".to_owned())
    } else if pairs == 0 && live.is_some() {
        Some("What the search verifies can be relinked once it ends".to_owned())
    } else if pairs == 0 {
        Some("Nothing verified to relink".to_owned())
    } else {
        None
    };
    Some(BarModel {
        verified: thousands(counts.found as u32),
        detail: (!detail.is_empty()).then(|| format!("\u{b7} {}", detail.join(" \u{b7} "))),
        stop: live.is_some_and(|(_, search)| matches!(search.status, SearchStatus::Running { .. })),
        relink: format!("Relink {}", thousands(pairs as u32)),
        pairs: if state.relinking { 0 } else { pairs },
        reason,
    })
}

fn info(state: &MissingState, list: &MissingOriginals, home: Option<&Path>) -> MissingInfo {
    let Some(asset) = &state.selected else {
        return MissingInfo::Nothing;
    };
    let Some((folder, search, row)) = state.row(asset) else {
        return MissingInfo::Nothing;
    };
    let group = list
        .groups
        .iter()
        .find(|group| &group.source_folder == folder);
    let facts = state
        .facts
        .as_ref()
        .filter(|(held, _)| held == asset)
        .map(|(_, facts)| facts);
    let was = match facts {
        Some(Ok(facts)) => short_path(&facts.was, home, INFO_PATH),
        _ => short_path(&folder.join(&row.file_name), home, INFO_PATH),
    };
    let chosen = search.chosen.get(asset);
    let (found, check, verified) = match &row.result {
        FindResult::Found { path } => (
            short_path(path, home, INFO_PATH),
            "Same size and fingerprint".to_owned(),
            true,
        ),
        FindResult::SeveralIdentical { paths } => match chosen {
            Some(path) => (
                short_path(path, home, INFO_PATH),
                "Same size and fingerprint".to_owned(),
                true,
            ),
            None => (
                format!("{} files", paths.len()),
                "Same size and fingerprint, several times".to_owned(),
                false,
            ),
        },
        FindResult::DifferentBytes { path } => (
            short_path(path, home, INFO_PATH),
            "Same name, different bytes".to_owned(),
            false,
        ),
        FindResult::Claimed { path, .. } => (
            short_path(path, home, INFO_PATH),
            "Same bytes, another photograph's file".to_owned(),
            false,
        ),
        FindResult::NotFound => (
            "\u{2014}".to_owned(),
            format!("Not under {}", short_path(&search.root, home, INFO_PATH)),
            false,
        ),
        FindResult::Checking => ("\u{2014}".to_owned(), "Checking\u{2026}".to_owned(), false),
    };
    let folder_names: Vec<String> = group
        .map(|group| {
            group
                .catalog_folders
                .iter()
                .map(|folder| folder.name.clone())
                .collect()
        })
        .unwrap_or_default();
    let folder_text = match folder_names.as_slice() {
        [one] => one.clone(),
        several => format!("One of {}", folders_text(several)),
    };
    let edits = match facts {
        Some(Ok(PhotoFacts { entries: 0, .. })) => "The Original alone".to_owned(),
        Some(Ok(PhotoFacts { entries, .. })) => format!(
            "{} {} \u{b7} kept as they are",
            entries,
            if *entries == 1 { "edit" } else { "edits" }
        ),
        Some(Err(error)) => format!("Unavailable: {error}"),
        None => "Reading\u{2026}".to_owned(),
    };
    let locate = ActionModel {
        label: "Locate a different file\u{2026}".to_owned(),
        reason: state
            .locating
            .as_ref()
            .map(|locating| format!("Locating {}\u{2026}", locating.file_name)),
    };
    MissingInfo::One(PhotoInfo {
        asset_id: asset.clone(),
        name: row.file_name.clone(),
        rows: vec![
            ("Was".to_owned(), was),
            ("Found".to_owned(), found),
            ("Check".to_owned(), check),
            ("Folder".to_owned(), folder_text),
            ("Edits".to_owned(), edits),
        ],
        verified,
        locate,
    })
}
