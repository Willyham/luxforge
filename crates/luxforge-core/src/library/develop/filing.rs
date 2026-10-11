//! Filing developed photographs (`docs/design/develop-from-folders.md`, "Folders Develop makes"):
//! the catalog folder each one goes into, resolved as it is committed, by its own camera-local
//! capture day. A place files it under its year, the place and its day ("2026 › Konstanz ›
//! 12 Sep"), or "Undated › Konstanz" with no capture time; an existing folder with `days` under
//! its day folder. Each folder of a path is found by its name among its siblings, ignoring case
//! as [`tree::same_name`] compares names, or made by the change that first puts a photograph in
//! it, recorded there as written (`catalog-folder`) before the photographs in it.
use crate::{
    Error,
    catalog_types::{CatalogFolderId, EventSpan, FolderValue, LibraryItem, LocalDay},
    library::{items, journal::Desired, tree},
};
use rusqlite::{OptionalExtension, Transaction};
use std::collections::HashMap;

/// The top-level folder a place's photographs with no capture time go into.
pub(crate) const UNDATED: &str = "Undated";

/// The catalog folder a Develop brings an event's photographs into.
#[derive(Clone, Debug)]
pub(crate) enum Destination {
    /// That folder; with `days`, each dated photograph's day folder inside it.
    Existing {
        folder_id: CatalogFolderId,
        days: bool,
    },
    /// A folder the Develop makes, as its first batch into it commits, with the spans of the
    /// events it is made from (none for an Undated one).
    New {
        folder: FolderValue,
        events: Vec<Option<EventSpan>>,
    },
    /// Year › `name` › day, for one event, whose span and identity a place folder it makes
    /// records (none for an Undated one).
    Place {
        name: String,
        event: Option<EventSpan>,
    },
}

/// A day folder's name: the day without a leading zero and the month in English, "3 Sep".
pub(crate) fn day_name(day: LocalDay) -> String {
    let (_, month, day) = day.ymd();
    format!("{day} {}", tree::MONTHS[month as usize - 1])
}

/// A year folder's name: "2026".
fn year_name(day: LocalDay) -> String {
    format!("{:04}", day.ymd().0)
}

/// The span a folder made for a Develop records: every dated event's it is made from, with the
/// event's identity when it is made from exactly one event.
pub(crate) fn made_from(events: &[Option<EventSpan>]) -> Option<EventSpan> {
    let dated: Vec<&EventSpan> = events.iter().flatten().collect();
    let start_ms = dated.iter().map(|span| span.start_ms).min()?;
    let end_ms = dated.iter().map(|span| span.end_ms).max()?;
    let event_id = match (events.len(), dated.as_slice()) {
        (1, [only]) => only.event_id.clone(),
        _ => None,
    };
    Some(EventSpan {
        start_ms,
        end_ms,
        event_id,
    })
}

/// Resolves the folder each photograph of one change goes into, in that change's transaction,
/// making the folders it needs as it goes: each is written at once, so the photographs that
/// follow find it, and kept in [`Self::changes`] for the change to record, parents first.
pub(crate) struct Filer<'a> {
    destination: &'a Destination,
    now_ms: i64,
    /// What each capture day (none: no capture time) resolved to.
    resolved: HashMap<Option<LocalDay>, CatalogFolderId>,
    /// The folders it made, as written: the change records them before its photographs.
    pub changes: Vec<(LibraryItem, Desired)>,
}

impl<'a> Filer<'a> {
    pub(crate) fn new(destination: &'a Destination, now_ms: i64) -> Self {
        Self {
            destination,
            now_ms,
            resolved: HashMap::new(),
            changes: Vec::new(),
        }
    }

    /// The folder a photograph captured on `day` (none with no capture time) goes into, found
    /// or made.
    pub(crate) fn folder(
        &mut self,
        tx: &Transaction<'_>,
        day: Option<LocalDay>,
    ) -> Result<CatalogFolderId, Error> {
        if let Some(known) = self.resolved.get(&day) {
            return Ok(known.clone());
        }
        let folder = match (self.destination, day) {
            (Destination::Existing { folder_id, .. }, None)
            | (
                Destination::Existing {
                    folder_id,
                    days: false,
                },
                _,
            ) => folder_id.clone(),
            (Destination::Existing { folder_id, .. }, Some(day)) => {
                self.found_or_made(tx, Some(folder_id), &day_name(day), None)?
            }
            (Destination::New { folder, .. }, _) => {
                let item = LibraryItem::CatalogFolder {
                    folder_id: folder.id.clone(),
                };
                if items::read(tx, &item)?.is_none() {
                    self.write(tx, folder)?;
                }
                folder.id.clone()
            }
            (Destination::Place { name, event }, day) => {
                let top = day.map_or_else(|| UNDATED.to_owned(), year_name);
                let top = self.found_or_made(tx, None, &top, None)?;
                let place = self.found_or_made(tx, Some(&top), name, event.clone())?;
                match day {
                    Some(day) => self.found_or_made(tx, Some(&place), &day_name(day), None)?,
                    None => place,
                }
            }
        };
        self.resolved.insert(day, folder.clone());
        Ok(folder)
    }

    /// The folder named `name` among `parent`'s (the top level's when none), ignoring case, or a
    /// new one made with `event`.
    fn found_or_made(
        &mut self,
        tx: &Transaction<'_>,
        parent: Option<&CatalogFolderId>,
        name: &str,
        event: Option<EventSpan>,
    ) -> Result<CatalogFolderId, Error> {
        if let Some(found) = sibling(tx, parent, name)? {
            return Ok(found);
        }
        let made = FolderValue {
            id: CatalogFolderId::new(),
            name: tree::checked_name(name)?,
            parent_id: parent.cloned(),
            created_ms: self.now_ms,
            event,
        };
        self.write(tx, &made)?;
        Ok(made.id)
    }

    fn write(&mut self, tx: &Transaction<'_>, folder: &FolderValue) -> Result<(), Error> {
        let item = LibraryItem::CatalogFolder {
            folder_id: folder.id.clone(),
        };
        let value = serde_json::to_value(folder)
            .map_err(|error| Error::internal(format!("cannot encode a folder: {error}")))?;
        items::write(tx, &item, Some(&value))?;
        self.changes.push((item, Desired::Written { before: None }));
        Ok(())
    }
}

/// The folder in `parent` (the top level when none) named `name`, ignoring case. One indexed
/// query over its siblings (`catalog_folder_names`).
pub(crate) fn sibling(
    connection: &rusqlite::Connection,
    parent: Option<&CatalogFolderId>,
    name: &str,
) -> Result<Option<CatalogFolderId>, Error> {
    let exact: Option<String> = connection
        .prepare_cached(
            "SELECT id FROM catalog_folders
             WHERE ifnull(parent_id, '') = ifnull(?1, '') AND name = ?2 COLLATE NOCASE",
        )?
        .query_row(
            rusqlite::params![parent.map(CatalogFolderId::as_str), name],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = exact {
        return Ok(Some(CatalogFolderId::parse(id)?));
    }
    if name.is_ascii() {
        return Ok(None);
    }
    // Beyond ASCII, `NOCASE` folds nothing: compare every sibling as a name is compared.
    let siblings: Vec<(String, String)> = connection
        .prepare_cached(
            "SELECT id, name FROM catalog_folders WHERE ifnull(parent_id, '') = ifnull(?1, '')",
        )?
        .query_map([parent.map(CatalogFolderId::as_str)], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<_, _>>()?;
    tree::clash(&siblings, name, None)
        .map(|(id, _)| CatalogFolderId::parse(id.clone()))
        .transpose()
}
