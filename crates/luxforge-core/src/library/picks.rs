//! Picks: a file marked to develop, kept by path and signature with its actor, request and time
//! until it is developed or cleared (`docs/design/catalog.md`, "Picking"). Picking and clearing are
//! library changes through the journal, one `pick` item per file; picking a file already picked
//! keeps its pick as it was. There are no ratings, flags or rejects.
use super::{
    journal::{Desired, Request},
    targets::NamedFile,
};
use crate::{
    Error,
    catalog_types::{LibraryChangeRow, LibraryItem, Pick, Volume, VolumeId},
    editor::library_rows::{PICK_COLUMNS, has_volume, read_pick},
    index::volume_of,
};
use rusqlite::Connection;
use std::{
    collections::HashMap,
    path::{MAIN_SEPARATOR, Path, PathBuf},
};

/// Which picks a page lists.
#[derive(Clone, Copy, Debug)]
pub(crate) enum PickScope<'a> {
    All,
    /// Picks of files in this folder on disk, and in its subfolders when asked.
    Folder {
        path: &'a Path,
        subfolders: bool,
    },
    /// Picks of files on this volume (a card).
    Volume(&'a VolumeId),
}

/// Picks in path order after `after`, at most `limit`.
pub(crate) fn page(
    connection: &Connection,
    scope: PickScope<'_>,
    after: Option<&Path>,
    limit: usize,
) -> Result<Vec<Pick>, Error> {
    let after = after.map_or_else(String::new, |path| path.to_string_lossy().into_owned());
    let limit = limit as i64;
    let picks = match scope {
        PickScope::All => connection
            .prepare_cached(&format!(
                "SELECT {PICK_COLUMNS} FROM picks WHERE path > ?1 ORDER BY path LIMIT ?2"
            ))?
            .query_map(rusqlite::params![after, limit], read_pick)?
            .collect::<Result<Vec<_>, _>>(),
        PickScope::Folder { path, subfolders } => {
            // Every path under the folder sorts between `folder/` and the separator's successor,
            // byte for byte as the catalog compares text.
            let folder = path.to_string_lossy();
            let folder = folder.trim_end_matches(MAIN_SEPARATOR);
            let low = format!("{folder}{MAIN_SEPARATOR}");
            let high = format!("{folder}{}", char::from(MAIN_SEPARATOR as u8 + 1));
            connection
                .prepare_cached(&format!(
                    "SELECT {PICK_COLUMNS} FROM picks
                     WHERE path > ?1 AND path >= ?3 AND path < ?4
                         AND (?5 OR instr(substr(path, length(?3) + 1), ?6) = 0)
                     ORDER BY path LIMIT ?2"
                ))?
                .query_map(
                    rusqlite::params![
                        after,
                        limit,
                        low,
                        high,
                        subfolders,
                        MAIN_SEPARATOR.to_string()
                    ],
                    read_pick,
                )?
                .collect::<Result<Vec<_>, _>>()
        }
        PickScope::Volume(volume) => connection
            .prepare_cached(&format!(
                "SELECT {PICK_COLUMNS} FROM picks WHERE path > ?1 AND volume_id = ?3
                 ORDER BY path LIMIT ?2"
            ))?
            .query_map(rusqlite::params![after, limit, volume.as_str()], read_pick)?
            .collect::<Result<Vec<_>, _>>(),
    };
    Ok(picks?)
}

/// A pick change, planned: what each pick item is to become, and the volumes the new picks are on,
/// which the change records beside them.
pub(crate) struct PickChange {
    pub changes: Vec<(LibraryItem, Desired)>,
    pub volumes: Vec<Volume>,
}

/// What picking (`picked`) or clearing `files` sets. Picking needs each file's signature and
/// volume: the volume a mounted folder is on now, or, for a folder that is not mounted, the
/// catalog's record of the volume the index names; a file with neither is refused. Clearing needs
/// only the path.
pub(crate) fn changes(
    connection: &Connection,
    files: Vec<NamedFile>,
    picked: bool,
    request: Request<'_>,
    now_ms: i64,
) -> Result<PickChange, Error> {
    if !picked {
        let changes = files
            .into_iter()
            .map(|file| (LibraryItem::Pick { path: file.path }, Desired::Value(None)))
            .collect();
        return Ok(PickChange {
            changes,
            volumes: Vec::new(),
        });
    }
    let mut folders: HashMap<PathBuf, Option<Volume>> = HashMap::new();
    let mut volumes: Vec<Volume> = Vec::new();
    let mut changes = Vec::with_capacity(files.len());
    for file in files {
        let Some(signature) = file.signature else {
            return Err(Error::source_unavailable(format!(
                "{} cannot be picked: it is not listed and cannot be read",
                file.path.display()
            )));
        };
        let folder = file.path.parent().unwrap_or(Path::new("")).to_path_buf();
        let mounted = folders
            .entry(folder)
            .or_insert_with_key(|folder| volume_of(folder, now_ms).ok())
            .clone();
        let volume_id = match (mounted, file.volume_id) {
            (Some(volume), _) => {
                if !volumes.iter().any(|known| known.id == volume.id) {
                    volumes.push(volume.clone());
                }
                volume.id
            }
            (None, Some(listed)) if has_volume(connection, &listed)? => listed,
            _ => {
                return Err(Error::source_unavailable(format!(
                    "{} cannot be picked: its volume is not connected",
                    file.path.display()
                )));
            }
        };
        let pick = Pick {
            path: file.path.clone(),
            signature,
            volume_id,
            actor: request.actor.to_owned(),
            request_id: request.request_id.to_owned(),
            picked_ms: now_ms,
            file_id: None,
        };
        let value = serde_json::to_value(&pick)
            .map_err(|error| Error::internal(format!("cannot encode a pick: {error}")))?;
        changes.push((
            LibraryItem::Pick { path: file.path },
            Desired::UnlessPresent(value),
        ));
    }
    Ok(PickChange { changes, volumes })
}

/// A pick change's label: "Picked L1003206.DNG", "Picked 18 files", "Cleared the pick of
/// L1003206.DNG", "Cleared 5 picks".
pub(crate) fn label(rows: &[LibraryChangeRow]) -> String {
    let picked = rows.iter().filter(|row| row.after.is_some()).count();
    let cleared = rows.len() - picked;
    let name = |row: &LibraryChangeRow| match &row.item {
        LibraryItem::Pick { path } => path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into(),
        ),
        other => other.key(),
    };
    match (picked, cleared, rows) {
        (1, 0, [row]) => format!("Picked {}", name(row)),
        (0, 1, [row]) => format!("Cleared the pick of {}", name(row)),
        (picked, 0, _) => format!("Picked {picked} files"),
        (0, cleared, _) => format!("Cleared {cleared} picks"),
        (picked, cleared, _) => format!("Picked {picked} files and cleared {cleared} picks"),
    }
}
