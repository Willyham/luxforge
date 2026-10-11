//! Filing developed photographs (`docs/design/develop-from-folders.md`, "Folders Develop makes"):
//! the catalog folder each one goes into, resolved as it is committed, by its own camera-local
//! capture day. A place files it under its year, the place and its day ("2026 › Konstanz ›
//! 12 Sep"), or "Undated › Konstanz" with no capture time; an existing folder with `days` under
//! its day folder. Each folder of a path is found by its name among its siblings, ignoring case
//! as [`tree::same_name`] compares names, or made by the change that first puts a photograph in
//! it, recorded there as written (`catalog-folder`) before the photographs in it.
use crate::{
    Error,
    catalog_types::{
        CatalogFolderId, EventSpan, FolderChoice, FolderValue, LibraryItem, LocalDay,
    },
    editor::library_rows::catalog_folder,
    library::{
        folders, items,
        journal::{self, Desired, Outcome, Request},
        tree,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction};
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

impl Destination {
    /// The destination `choice` names for photographs of the event `event` spans (none for an
    /// undated one): an existing folder must be in the catalog; a place's and a new folder's
    /// name follow a folder's rules, and a new folder's must be free among its siblings, which it
    /// is made in with `event`.
    pub(crate) fn chosen(
        catalog: &Connection,
        choice: &FolderChoice,
        event: Option<EventSpan>,
        now_ms: i64,
    ) -> Result<Self, Error> {
        Ok(match choice {
            FolderChoice::Existing { folder_id, days } => {
                folders::folder(catalog, folder_id)?;
                Self::Existing {
                    folder_id: folder_id.clone(),
                    days: *days,
                }
            }
            FolderChoice::Place { name } => Self::Place {
                name: tree::checked_name(name)?,
                event,
            },
            FolderChoice::New { name, parent_id } => {
                let (mut folder, _) = folders::create(catalog, name, parent_id.as_ref(), now_ms)?;
                folder.event = event.clone();
                Self::New {
                    folder,
                    events: vec![event],
                }
            }
        })
    }

    /// Whether `choice` names this destination, so events given it share it: the same existing
    /// folder with the same `days`, or the same new folder (its name ignoring case, under the same
    /// parent). A place is never shared: its folders are found by name as photographs are filed.
    pub(crate) fn shares(&self, choice: &FolderChoice) -> bool {
        match (self, choice) {
            (
                Self::Existing { folder_id, days },
                FolderChoice::Existing {
                    folder_id: chosen,
                    days: with,
                },
            ) => folder_id == chosen && days == with,
            (Self::New { folder, .. }, FolderChoice::New { name, parent_id }) => {
                folder.parent_id == *parent_id && tree::same_name(&folder.name, name.trim())
            }
            _ => false,
        }
    }

    /// The folder a photograph captured on `day` (none with no capture time) would go into when
    /// every folder on its path is already in the catalog, found as [`Filer::folder`] finds it,
    /// reading only; none when filing it would make a folder.
    pub(crate) fn found(
        &self,
        connection: &Connection,
        day: Option<LocalDay>,
    ) -> Result<Option<CatalogFolderId>, Error> {
        match (self, day) {
            (Self::Existing { folder_id, .. }, None)
            | (
                Self::Existing {
                    folder_id,
                    days: false,
                },
                _,
            ) => Ok(Some(folder_id.clone())),
            (Self::Existing { folder_id, .. }, Some(day)) => {
                sibling(connection, Some(folder_id), &day_name(day))
            }
            (Self::New { folder, .. }, _) => Ok(catalog_folder(connection, &folder.id)?
                .is_some()
                .then(|| folder.id.clone())),
            (Self::Place { name, .. }, day) => {
                let top = day.map_or_else(|| UNDATED.to_owned(), year_name);
                let Some(top) = sibling(connection, None, &top)? else {
                    return Ok(None);
                };
                let Some(place) = sibling(connection, Some(&top), name)? else {
                    return Ok(None);
                };
                match day {
                    Some(day) => sibling(connection, Some(&place), &day_name(day)),
                    None => Ok(Some(place)),
                }
            }
        }
    }
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
    /// The names of the folders it made, parents first.
    made: Vec<String>,
}

impl<'a> Filer<'a> {
    pub(crate) fn new(destination: &'a Destination, now_ms: i64) -> Self {
        Self {
            destination,
            now_ms,
            resolved: HashMap::new(),
            changes: Vec::new(),
            made: Vec::new(),
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
        self.made.push(folder.name.clone());
        Ok(())
    }
}

/// The folder in `parent` (the top level when none) named `name`, ignoring case. One indexed
/// query over its siblings (`catalog_folder_names`).
pub(crate) fn sibling(
    connection: &Connection,
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

/// File the one photograph a merge publishes, captured on `day` (none with no capture time), by
/// `destination`, in the publication's transaction: answers the folder it goes into. The folders
/// it needs are made first and recorded as a library change of their own by the publication's
/// actor under its request identity ("folder.create", "Created folders 2026 › Konstanz ›
/// 12 Sep"), before the publication's change, so that undoing the publication — which keeps its
/// result, in Removed, in that folder — leaves them, as it leaves a folder made before it.
pub(crate) fn file_one(
    tx: &Transaction<'_>,
    request: Request<'_>,
    destination: &Destination,
    day: Option<LocalDay>,
    now_ms: i64,
) -> Result<(CatalogFolderId, Option<Outcome>), Error> {
    let mut filer = Filer::new(destination, now_ms);
    let folder = filer.folder(tx, day)?;
    if filer.changes.is_empty() {
        return Ok((folder, None));
    }
    let label = match filer.made.as_slice() {
        [one] => format!("Created folder {one}"),
        many => format!("Created folders {}", many.join(" › ")),
    };
    let request = Request {
        method: MADE_FOR_MERGE,
        ..request
    };
    let outcome = journal::apply_part(tx, request, filer.changes, |_| label)?;
    Ok((folder, Some(outcome)))
}

/// The method a merge's folders are recorded under, apart from its publication's change.
pub(crate) const MADE_FOR_MERGE: &str = "folder.create";
