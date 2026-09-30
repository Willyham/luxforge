//! Reading a source's items: one pass over compact columns of the index's `files` rows or the
//! catalog's `assets` and `capture` rows, in row order, into [`FrameFacts`] and the few more facts
//! views filter, sort and count by ([`Extra`]). Every folder, body, lens and place is stored once
//! however many items share it ([`Names`]), so an item costs its name and a few words.
//!
//! A file's pick, its developed photograph and its availability are read per folder of the source,
//! with range queries on the catalog's `picks` and `assets` keys and the index's offline roots,
//! never one query per file.

use super::{EventCache, filter::Filter, json_list, local_day, places, subtree_bounds, within};
use crate::{
    EditorService, Error, SourceTag,
    catalog_types::{
        AssetRowId, BodyIndex, CameraBody, CatalogFolderId, CollectionId, CollectionKind, Exposure,
        FileAvailability, FileId, FolderIndex, FrameFacts, FrameTables, GeoPosition, PlaceNames,
        SortKey, Thresholds, ViewFilter, ViewItem, ViewQuery, ViewSource,
    },
};
use rusqlite::{OptionalExtension, Row, ToSql, types::ValueRef};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, Path, PathBuf},
};

const DAY_MS: i64 = 86_400_000;

/// What views read of an item beside its [`FrameFacts`].
#[derive(Clone, Copy, Debug)]
pub(super) struct Extra {
    pub kind: SourceTag,
    /// An index into [`Names::lenses`].
    pub lens: Option<u32>,
    /// An index into [`Names::places`].
    pub place: Option<u32>,
    /// A file's pick; false for a photograph.
    pub picked: bool,
    /// A file that is already a developed photograph's original; true for a photograph.
    pub in_catalog: bool,
    /// A photograph with history beyond its Original, when the query needs it; false for a file.
    pub edited: bool,
    /// When a photograph was developed; 0 for a file.
    pub developed_ms: i64,
    /// A photograph's newest history entry, or when it was developed if none since, when the
    /// query needs it.
    pub last_edited_ms: i64,
    pub availability: FileAvailability,
    /// A file that Moments without a pick leaves out: picked, or in a moment with a picked frame
    /// (`view::decide`).
    pub decided: bool,
}

/// The folders, bodies, lenses and places of a source's items, each stored once.
#[derive(Default)]
pub(super) struct Names {
    pub tables: FrameTables,
    folder_index: HashMap<String, FolderIndex>,
    /// Each folder as stored, by [`FolderIndex`].
    pub folders: Vec<String>,
    body_index: HashMap<String, BodyIndex>,
    /// How many bodies [`Self::tables`] holds; every [`BodyIndex`] is below it.
    pub bodies: usize,
    lens_index: HashMap<String, u32>,
    pub lenses: Vec<String>,
    place_index: HashMap<String, u32>,
    pub places: Vec<String>,
    key: String,
}

impl Names {
    fn folder(&mut self, folder: &str) -> FolderIndex {
        if let Some(index) = self.folder_index.get(folder) {
            return *index;
        }
        let index = self.tables.folder(PathBuf::from(folder));
        self.folder_index.insert(folder.to_owned(), index);
        if self.folders.len() <= index.0 as usize {
            self.folders.resize(index.0 as usize + 1, String::new());
        }
        self.folders[index.0 as usize] = folder.to_owned();
        index
    }

    fn body(&mut self, make: Option<&str>, model: Option<&str>, serial: Option<&str>) -> BodyIndex {
        self.key.clear();
        if let (Some(make), Some(model)) = (make, model) {
            self.key.push_str(make);
            self.key.push('\0');
            self.key.push_str(model);
            self.key.push('\0');
            self.key.push_str(serial.unwrap_or(""));
        }
        if let Some(index) = self.body_index.get(self.key.as_str()) {
            return *index;
        }
        let camera = make.zip(model).map(|(make, model)| CameraBody {
            make: make.to_owned(),
            model: model.to_owned(),
            serial: serial.map(str::to_owned),
        });
        let index = self.tables.body(camera.as_ref());
        self.bodies = self.bodies.max(index.0 as usize + 1);
        self.body_index.insert(self.key.clone(), index);
        index
    }

    fn lens(&mut self, lens: Option<&str>) -> Option<u32> {
        let lens = lens?;
        if let Some(index) = self.lens_index.get(lens) {
            return Some(*index);
        }
        let index = self.lenses.len() as u32;
        self.lenses.push(lens.to_owned());
        self.lens_index.insert(lens.to_owned(), index);
        Some(index)
    }

    fn place(&mut self, place: Option<&str>) -> Option<u32> {
        let place = place?;
        if let Some(index) = self.place_index.get(place) {
            return Some(*index);
        }
        let index = self.places.len() as u32;
        self.places.push(place.to_owned());
        self.place_index.insert(place.to_owned(), index);
        Some(index)
    }
}

/// A source's items, ready to filter, order and count: `facts` in any order, and each item's
/// [`Extra`] found by its identity.
pub(super) struct Candidates {
    pub over_files: bool,
    /// Every item, ascending; `extras` is aligned with it.
    keys: Vec<ViewItem>,
    extras: Vec<Extra>,
    /// Aligned with `keys` as read; views reorder them.
    pub facts: Vec<FrameFacts>,
    pub names: Names,
}

impl Candidates {
    /// The facts of the item `item`, which is one of these.
    pub fn extra(&self, item: ViewItem) -> &Extra {
        &self.extras[self.position(item)]
    }

    pub fn extra_mut(&mut self, item: ViewItem) -> &mut Extra {
        let position = self.position(item);
        &mut self.extras[position]
    }

    fn position(&self, item: ViewItem) -> usize {
        self.keys
            .binary_search(&item)
            .expect("a candidate's facts belong to one of its items")
    }

    /// Keep the items `keep` accepts. Only before `facts` are reordered, while they are aligned
    /// with the items.
    fn retain(&mut self, mut keep: impl FnMut(&FrameFacts, &Extra) -> bool) {
        debug_assert!(
            self.facts
                .iter()
                .zip(&self.keys)
                .all(|(fact, key)| fact.item == *key)
        );
        let mask: Vec<bool> = self
            .facts
            .iter()
            .zip(&self.extras)
            .map(|(fact, extra)| keep(fact, extra))
            .collect();
        let mut kept = mask.iter();
        self.keys.retain(|_| *kept.next().expect("aligned"));
        let mut kept = mask.iter();
        self.extras.retain(|_| *kept.next().expect("aligned"));
        let mut kept = mask.iter();
        self.facts.retain(|_| *kept.next().expect("aligned"));
    }
}

/// What reading needs beyond the columns every view reads.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Needs {
    /// Photographs' history: whether each is edited and when it was last edited.
    pub edits: bool,
}

impl Needs {
    pub fn of(filter: &ViewFilter, sort: SortKey) -> Self {
        Self {
            edits: filter.edited.is_some() || sort == SortKey::LastEdited,
        }
    }

    fn with(self, other: Self) -> Self {
        Self {
            edits: self.edits || other.edits,
        }
    }
}

/// How a source is read: the service, the event cache an event source resolves through, the index
/// revision and thresholds it resolves under, and the most items a source may hold.
pub(super) struct Reader<'a> {
    pub service: &'a EditorService,
    pub events: &'a mut EventCache,
    pub index_revision: u64,
    pub thresholds: &'a Thresholds,
    pub limit: usize,
}

/// Read `source`'s items. Refused with `resource-limit` past the reader's limit, and with
/// `validation` for an event, card, catalog folder or collection that does not exist, a folder
/// that is not an absolute path, a collection group, and a smart collection whose stored query is
/// over files or names another smart collection.
pub(super) fn read(
    reader: &mut Reader<'_>,
    source: &ViewSource,
    needs: Needs,
) -> Result<Candidates, Error> {
    read_source(reader, source, needs, false)
}

fn read_source(
    reader: &mut Reader<'_>,
    source: &ViewSource,
    needs: Needs,
    in_smart: bool,
) -> Result<Candidates, Error> {
    let now = crate::editor::now_ms();
    match source {
        ViewSource::Event { event_id } => {
            let files = reader.events.files(
                reader.service,
                reader.index_revision,
                reader.thresholds,
                event_id,
            )?;
            read_files(reader, &[FileScope::Ids(&files)])
        }
        ViewSource::Folder { path, subfolders } => {
            let folder = folder_text(path)?;
            read_files(
                reader,
                &[FileScope::Folder {
                    path: &folder,
                    subfolders: *subfolders,
                }],
            )
        }
        ViewSource::Card { volume_id } => {
            let roots = card_roots(reader.service, volume_id.as_str())?;
            let scopes: Vec<_> = roots
                .iter()
                .map(|root| FileScope::Folder {
                    path: root,
                    subfolders: true,
                })
                .collect();
            read_files(reader, &scopes)
        }
        ViewSource::AllPhotographs => read_photos(reader, PhotoScope::All, needs),
        ViewSource::RecentlyDeveloped { days } => read_photos(
            reader,
            PhotoScope::Since(now.saturating_sub(i64::from(*days).saturating_mul(DAY_MS))),
            needs,
        ),
        ViewSource::CatalogFolder {
            folder_id,
            subfolders,
        } => {
            let exists: bool = reader.service.connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM catalog_folders WHERE id = ?1)",
                [folder_id.as_str()],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(Error::validation(format!(
                    "unknown catalog folder {folder_id}"
                )));
            }
            read_photos(
                reader,
                PhotoScope::Folder {
                    folder_id,
                    subfolders: *subfolders,
                },
                needs,
            )
        }
        ViewSource::Collection { collection_id } => {
            let (kind, stored) = collection(reader.service, collection_id)?;
            match kind {
                CollectionKind::Collection => {
                    read_photos(reader, PhotoScope::Collection(collection_id), needs)
                }
                CollectionKind::Group => Err(Error::validation(format!(
                    "{collection_id} is a collection group, which holds collections, not photographs"
                ))),
                CollectionKind::Smart if in_smart => Err(Error::validation(format!(
                    "a smart collection's query names smart collection {collection_id}; a smart collection may not name another"
                ))),
                CollectionKind::Smart => {
                    let stored = stored.ok_or_else(|| {
                        Error::incompatible(format!(
                            "smart collection {collection_id} has no query"
                        ))
                    })?;
                    if stored.source.over_files() {
                        return Err(Error::validation(format!(
                            "smart collection {collection_id}'s query is over files, not photographs"
                        )));
                    }
                    stored.validate()?;
                    let needs = needs.with(Needs::of(&stored.filter, SortKey::CaptureTime));
                    let mut candidates = read_source(reader, &stored.source, needs, true)?;
                    let filter = Filter::new(&stored.filter, &candidates);
                    candidates.retain(|fact, extra| filter.failing(fact, extra) == 0);
                    Ok(candidates)
                }
            }
        }
        ViewSource::MissingOriginals => read_photos(reader, PhotoScope::Missing, needs),
        ViewSource::Removed => read_photos(reader, PhotoScope::Removed, needs),
    }
}

/// The files a source over files covers, ascending, reading only their rows' keys.
pub(super) fn source_files(
    service: &EditorService,
    events: &mut EventCache,
    source: &ViewSource,
) -> Result<Vec<FileId>, Error> {
    let (folders, subfolders) = match source {
        ViewSource::Event { event_id } => {
            let revision = super::index_revision(service.index()?.connection())?;
            return events.files(service, revision, &Thresholds::default(), event_id);
        }
        ViewSource::Folder { path, subfolders } => (vec![folder_text(path)?], *subfolders),
        ViewSource::Card { volume_id } => (card_roots(service, volume_id.as_str())?, true),
        _ => {
            return Err(Error::validation(
                "the source is over developed photographs, not files",
            ));
        }
    };
    let index = service.index()?;
    let mut files = Vec::new();
    for folder in &folders {
        let scope = FileScope::Folder {
            path: folder,
            subfolders,
        };
        let (sql, parameters) = scope.statement("id");
        let mut statement = index.connection().prepare_cached(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(parameters.iter()))?;
        while let Some(row) = rows.next()? {
            files.push(FileId(row.get(0)?));
        }
    }
    files.sort_unstable();
    files.dedup();
    Ok(files)
}

/// A folder on disk as the index stores it: absolute, without `.`, `..` or a trailing separator.
fn folder_text(path: &Path) -> Result<String, Error> {
    if !path.is_absolute() || path.components().any(|part| part == Component::ParentDir) {
        return Err(Error::validation(format!(
            "a folder on disk is named by its absolute path: {}",
            path.display()
        )));
    }
    let normalized: PathBuf = path.components().collect();
    Ok(normalized.to_string_lossy().into_owned())
}

/// The roots the index lists for the card on `volume`; refused when it lists none.
fn card_roots(service: &EditorService, volume: &str) -> Result<Vec<String>, Error> {
    let index = service.index()?;
    let mut statement = index.connection().prepare_cached(
        "SELECT path FROM roots WHERE kind = 'card' AND volume_id = ?1 ORDER BY path",
    )?;
    let roots = statement
        .query_map([volume], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    if roots.is_empty() {
        return Err(Error::validation(format!(
            "no card on volume {volume} is indexed"
        )));
    }
    Ok(roots)
}

/// A collection's kind and, for a smart collection, its stored query.
fn collection(
    service: &EditorService,
    id: &CollectionId,
) -> Result<(CollectionKind, Option<ViewQuery>), Error> {
    let (kind, query): (String, Option<String>) = service
        .connection
        .query_row(
            "SELECT kind, query_json FROM collections WHERE id = ?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| Error::validation(format!("unknown collection {id}")))?;
    let kind = CollectionKind::parse(&kind)
        .ok_or_else(|| Error::incompatible(format!("collection {id}: unknown kind {kind}")))?;
    let query = query
        .map(|query| crate::editor::decode("invalid smart collection query", query))
        .transpose()?;
    Ok((kind, query))
}

// ── Files ────────────────────────────────────────────────────────────────────────────────────

/// Which files of the index to read.
#[derive(Clone, Copy)]
pub(super) enum FileScope<'a> {
    Ids(&'a [FileId]),
    /// The files directly in a folder, or in it and every folder inside it.
    Folder {
        path: &'a str,
        subfolders: bool,
    },
}

impl FileScope<'_> {
    /// The statement reading `columns` of the scope's files in row order, and its parameters.
    pub fn statement(&self, columns: &str) -> (String, Vec<String>) {
        match *self {
            Self::Ids(ids) => (
                format!(
                    "SELECT {columns} FROM files WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY id"
                ),
                vec![json_list(&ids.iter().map(|id| id.0).collect::<Vec<_>>()).unwrap_or_default()],
            ),
            Self::Folder {
                path,
                subfolders: false,
            } => (
                format!("SELECT {columns} FROM files WHERE folder = ?1 ORDER BY id"),
                vec![path.to_owned()],
            ),
            Self::Folder {
                path,
                subfolders: true,
            } => {
                let (lower, upper) = subtree_bounds(path);
                (
                    format!(
                        "SELECT {columns} FROM files WHERE folder = ?1 OR (folder >= ?2 AND folder < ?3) ORDER BY id"
                    ),
                    vec![path.to_owned(), lower, upper],
                )
            }
        }
    }
}

/// The `files` columns [`file_row`] reads, in its order.
pub(super) const FILE_COLUMNS: &str = "id, folder, name, kind, capture_ms, offset_minutes, \
    latitude, longitude, altitude_m, make, model, body_serial, lens, exposure_time_s, f_number, \
    iso, exposure_bias_ev, focal_mm, focal_35mm_mm, path";

/// One `files` row of [`FILE_COLUMNS`]: its frame facts, kind, lens and place, and its path.
pub(super) struct FileRow {
    pub fact: FrameFacts,
    pub kind: SourceTag,
    pub lens: Option<u32>,
    pub place: Option<u32>,
    pub path: Box<str>,
}

pub(super) fn file_row(
    row: &Row<'_>,
    names: &mut Names,
    places: &dyn PlaceNames,
) -> Result<FileRow, Error> {
    let id: i64 = row.get(0)?;
    let folder = names.folder(text(row, 1)?);
    let name: Box<str> = text(row, 2)?.into();
    let kind = text(row, 3)?;
    let kind = SourceTag::parse(kind)
        .ok_or_else(|| Error::incompatible(format!("index file row {id}: unknown kind {kind}")))?;
    let instant: Option<i64> = row.get(4)?;
    let offset: Option<i16> = row.get(5)?;
    let position = position(row, 6)?;
    let body = names.body(
        optional_text(row, 9)?,
        optional_text(row, 10)?,
        optional_text(row, 11)?,
    );
    let lens = names.lens(optional_text(row, 12)?);
    let exposure = exposure(row, 13)?;
    let place = position
        .as_ref()
        .and_then(|position| places.nearest(position));
    let place = names.place(place.as_deref());
    Ok(FileRow {
        fact: FrameFacts {
            item: ViewItem::File(FileId(id)),
            folder,
            name,
            instant_ms: instant,
            local_day: instant.map(|instant| local_day(instant, offset)),
            position,
            body,
            exposure,
        },
        kind,
        lens,
        place,
        path: text(row, 19)?.into(),
    })
}

/// Read the files of `scopes`, each once, and what the catalog and the index say about them.
fn read_files(reader: &mut Reader<'_>, scopes: &[FileScope<'_>]) -> Result<Candidates, Error> {
    let service = reader.service;
    let index = service.index()?;
    let connection = index.connection();
    let places = places();
    let mut names = Names::default();
    let mut read: Vec<(FrameFacts, Extra, Box<str>)> = Vec::new();
    for scope in scopes {
        let (sql, parameters) = scope.statement(FILE_COLUMNS);
        let mut statement = connection.prepare_cached(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(parameters.iter()))?;
        while let Some(row) = rows.next()? {
            if read.len() == reader.limit {
                return Err(too_many(reader.limit));
            }
            let file = file_row(row, &mut names, places)?;
            read.push((
                file.fact,
                Extra {
                    kind: file.kind,
                    lens: file.lens,
                    place: file.place,
                    picked: false,
                    in_catalog: false,
                    edited: false,
                    developed_ms: 0,
                    last_edited_ms: 0,
                    availability: FileAvailability::Available,
                    decided: false,
                },
                file.path,
            ));
        }
    }
    if scopes.len() > 1 {
        read.sort_unstable_by_key(|(fact, _, _)| fact.item);
        read.dedup_by_key(|(fact, _, _)| fact.item);
    }
    // Offline roots, by folder.
    let offline: Vec<String> = connection
        .prepare_cached("SELECT path FROM roots WHERE offline = 1")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let folder_offline: Vec<bool> = names
        .folders
        .iter()
        .map(|folder| offline.iter().any(|root| within(folder, root)))
        .collect();
    drop(index);
    // Picks and developed originals in the source's folders, one range query each per subtree.
    let mut picked = HashSet::new();
    let mut developed = HashSet::new();
    let mut picks = service
        .connection
        .prepare_cached("SELECT path FROM picks WHERE path >= ?1 AND path < ?2")?;
    let mut originals = service.connection.prepare_cached(
        "SELECT canonical_locator FROM assets
         WHERE canonical_locator >= ?1 AND canonical_locator < ?2 AND removed_ms IS NULL",
    )?;
    for folder in outermost(&names.folders) {
        let (lower, upper) = subtree_bounds(folder);
        for path in picks.query_map([&lower, &upper], |row| row.get::<_, String>(0))? {
            picked.insert(path?);
        }
        for path in originals.query_map([&lower, &upper], |row| row.get::<_, String>(0))? {
            developed.insert(path?);
        }
    }
    let mut candidates = Candidates {
        over_files: true,
        keys: Vec::with_capacity(read.len()),
        extras: Vec::with_capacity(read.len()),
        facts: Vec::with_capacity(read.len()),
        names,
    };
    for (fact, mut extra, path) in read {
        extra.picked = picked.contains(&*path);
        extra.in_catalog = developed.contains(&*path);
        if folder_offline[fact.folder.0 as usize] {
            extra.availability = FileAvailability::Offline;
        }
        candidates.keys.push(fact.item);
        candidates.extras.push(extra);
        candidates.facts.push(fact);
    }
    Ok(candidates)
}

/// The folders of `folders` that no other of them contains, so one range query each covers every
/// one of them.
fn outermost(folders: &[String]) -> Vec<&str> {
    let mut sorted: Vec<&str> = folders.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    let mut kept: Vec<&str> = Vec::new();
    // Sorted, a folder follows every folder that contains it, though not always directly: `/a/b-x`
    // sorts between `/a/b` and `/a/b/c`.
    for folder in sorted {
        if !kept.iter().any(|outer| within(folder, outer)) {
            kept.push(folder);
        }
    }
    kept
}

// ── Photographs ──────────────────────────────────────────────────────────────────────────────

/// Which developed photographs to read.
enum PhotoScope<'a> {
    /// Every photograph but the removed.
    All,
    /// Those developed at or after this instant, but the removed.
    Since(i64),
    Folder {
        folder_id: &'a CatalogFolderId,
        subfolders: bool,
    },
    Collection(&'a CollectionId),
    /// Those whose original is not available, but the removed.
    Missing,
    Removed,
}

impl PhotoScope<'_> {
    /// A leading common table expression, the condition on `assets a`, and the one parameter.
    fn condition(&self) -> (&'static str, &'static str, Option<Box<dyn ToSql + '_>>) {
        match self {
            Self::All => ("", "a.removed_ms IS NULL", None),
            Self::Since(since) => (
                "",
                "a.removed_ms IS NULL AND a.developed_ms >= ?1",
                Some(Box::new(*since)),
            ),
            Self::Folder {
                folder_id,
                subfolders: false,
            } => (
                "",
                "a.removed_ms IS NULL AND a.catalog_folder_id = ?1",
                Some(Box::new(folder_id.as_str())),
            ),
            Self::Folder {
                folder_id,
                subfolders: true,
            } => (
                "WITH RECURSIVE tree(id) AS (SELECT ?1 UNION SELECT f.id FROM catalog_folders f JOIN tree t ON f.parent_id = t.id) ",
                "a.removed_ms IS NULL AND a.catalog_folder_id IN (SELECT id FROM tree)",
                Some(Box::new(folder_id.as_str())),
            ),
            Self::Collection(collection) => (
                "",
                "a.removed_ms IS NULL AND a.row_id IN (SELECT asset_row FROM collection_members WHERE collection_id = ?1)",
                Some(Box::new(collection.as_str())),
            ),
            Self::Missing => (
                "",
                "a.removed_ms IS NULL AND a.availability <> 'available'",
                None,
            ),
            Self::Removed => ("", "a.removed_ms IS NOT NULL", None),
        }
    }
}

/// A photograph's newest history entry after its Original, or none: whether it is edited and when.
const LAST_EDIT: &str =
    "(SELECT MAX(e.timestamp_ms) FROM entries e WHERE e.asset_id = a.id AND e.sequence > 0)";

fn read_photos(
    reader: &mut Reader<'_>,
    scope: PhotoScope<'_>,
    needs: Needs,
) -> Result<Candidates, Error> {
    let (with, condition, parameter) = scope.condition();
    let edits = if needs.edits { LAST_EDIT } else { "NULL" };
    let sql = format!(
        "{with}SELECT a.row_id, a.source_folder, a.file_name, a.source_kind, a.availability,
             a.developed_ms, c.capture_ms, c.offset_minutes, c.latitude, c.longitude, c.altitude_m,
             c.make, c.model, c.body_serial, c.lens, c.exposure_time_s, c.f_number, c.iso,
             c.exposure_bias_ev, c.focal_mm, c.focal_35mm_mm, c.place, {edits}
         FROM assets a LEFT JOIN capture c ON c.asset_row = a.row_id
         WHERE {condition} ORDER BY a.row_id"
    );
    let mut statement = reader.service.connection.prepare_cached(&sql)?;
    let parameters: Vec<&dyn ToSql> = parameter.iter().map(|value| value.as_ref()).collect();
    let mut rows = statement.query(parameters.as_slice())?;
    let mut candidates = Candidates {
        over_files: false,
        keys: Vec::new(),
        extras: Vec::new(),
        facts: Vec::new(),
        names: Names::default(),
    };
    while let Some(row) = rows.next()? {
        if candidates.keys.len() == reader.limit {
            return Err(too_many(reader.limit));
        }
        let names = &mut candidates.names;
        let row_id: i64 = row.get(0)?;
        let folder = names.folder(text(row, 1)?);
        let name: Box<str> = text(row, 2)?.into();
        let kind = text(row, 3)?;
        let kind = SourceTag::parse(kind).ok_or_else(|| {
            Error::incompatible(format!("photograph row {row_id}: unknown kind {kind}"))
        })?;
        let availability = text(row, 4)?;
        let availability = FileAvailability::parse(availability).ok_or_else(|| {
            Error::incompatible(format!(
                "photograph row {row_id}: unknown availability {availability}"
            ))
        })?;
        let developed_ms: i64 = row.get(5)?;
        let instant: Option<i64> = row.get(6)?;
        let offset: Option<i16> = row.get(7)?;
        let position = position(row, 8)?;
        let body = names.body(
            optional_text(row, 11)?,
            optional_text(row, 12)?,
            optional_text(row, 13)?,
        );
        let lens = names.lens(optional_text(row, 14)?);
        let exposure = exposure(row, 15)?;
        let place = names.place(optional_text(row, 21)?);
        let last_edit: Option<i64> = row.get(22)?;
        let item = ViewItem::Photo(AssetRowId(row_id));
        candidates.keys.push(item);
        candidates.extras.push(Extra {
            kind,
            lens,
            place,
            picked: false,
            in_catalog: true,
            edited: last_edit.is_some(),
            developed_ms,
            last_edited_ms: last_edit.unwrap_or(developed_ms),
            availability,
            decided: false,
        });
        candidates.facts.push(FrameFacts {
            item,
            folder,
            name,
            instant_ms: instant,
            local_day: instant.map(|instant| local_day(instant, offset)),
            position,
            body,
            exposure,
        });
    }
    Ok(candidates)
}

// ── Columns ──────────────────────────────────────────────────────────────────────────────────

fn too_many(limit: usize) -> Error {
    Error::resource_limit(format!(
        "the source holds more than the {limit} items a view may hold"
    ))
}

fn text<'r>(row: &'r Row<'_>, index: usize) -> Result<&'r str, Error> {
    optional_text(row, index)?
        .ok_or_else(|| Error::incompatible(format!("a stored row has no value in column {index}")))
}

fn optional_text<'r>(row: &'r Row<'_>, index: usize) -> Result<Option<&'r str>, Error> {
    match row.get_ref(index)? {
        ValueRef::Null => Ok(None),
        ValueRef::Text(bytes) => std::str::from_utf8(bytes)
            .map(Some)
            .map_err(|_| Error::incompatible(format!("column {index} is not UTF-8 text"))),
        _ => Err(Error::incompatible(format!("column {index} is not text"))),
    }
}

/// A position from latitude, longitude and altitude at `first`, `first + 1` and `first + 2`.
pub(super) fn position(row: &Row<'_>, first: usize) -> Result<Option<GeoPosition>, Error> {
    let lat: Option<f64> = row.get(first)?;
    let lon: Option<f64> = row.get(first + 1)?;
    let alt_m: Option<f64> = row.get(first + 2)?;
    Ok(lat
        .zip(lon)
        .map(|(lat, lon)| GeoPosition { lat, lon, alt_m }))
}

/// An exposure from its six columns from `first`: time, f-number, ISO, bias, focal length and its
/// 35 mm equivalent.
pub(super) fn exposure(row: &Row<'_>, first: usize) -> Result<Exposure, Error> {
    Ok(Exposure {
        time_s: row.get(first)?,
        f_number: row.get(first + 1)?,
        iso: row.get(first + 2)?,
        bias_ev: row.get(first + 3)?,
        focal_mm: row.get(first + 4)?,
        focal_35mm_mm: row.get(first + 5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browse_outermost_folders_cover_their_subfolders_once() {
        let folders: Vec<String> = ["/a/b/c", "/a/b", "/a/b-x", "/d", "/a/b/c/e"]
            .map(String::from)
            .to_vec();
        assert_eq!(outermost(&folders), ["/a/b", "/a/b-x", "/d"]);
    }

    #[test]
    fn browse_a_folder_is_named_absolutely() {
        assert_eq!(folder_text(Path::new("/a/b/")).unwrap(), "/a/b");
        assert_eq!(folder_text(Path::new("/a/./b")).unwrap(), "/a/b");
        assert!(folder_text(Path::new("a/b")).is_err());
        assert!(folder_text(Path::new("/a/../b")).is_err());
    }
}
