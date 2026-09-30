//! What the grid can draw for a row now ([`PreviewState`]), read from the index's preview records:
//! the seam to lane B's preview lane. Once its cache lands, [`grid_states`] answers with
//! `crate::previews::grid_states` over the same files, so the switch is that one call; until then
//! it reads the records the contracts laid down.

use super::json_list;
use crate::{
    AssetId, EntryId,
    catalog_types::{FileId, PreviewState},
};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

/// Each file's grid preview state, in the order of `files`: `ready` when the index records its grid
/// tier, `thumbnail` when its header records an embedded thumbnail, `unavailable` when its header
/// could not be read, and `pending` otherwise. A state is advisory — the preview lane reads what is
/// missing — so a file the index no longer has, or a read that fails, is `pending`.
pub(super) fn grid_states(index: &Connection, files: &[FileId]) -> Vec<PreviewState> {
    let read = || -> Result<HashMap<i64, PreviewState>, crate::Error> {
        let mut statement = index.prepare_cached(
            "SELECT f.id,
                 EXISTS(SELECT 1 FROM previews p WHERE p.file_id = f.id AND p.tier = 'grid'),
                 f.thumb_offset IS NOT NULL, f.header_state
             FROM files f WHERE f.id IN (SELECT value FROM json_each(?1))",
        )?;
        let ids: Vec<i64> = files.iter().map(|file| file.0).collect();
        let rows = statement.query_map([json_list(&ids)?], |row| {
            let state = if row.get::<_, bool>(1)? {
                PreviewState::Ready
            } else if row.get::<_, bool>(2)? {
                PreviewState::Thumbnail
            } else if row.get::<_, String>(3)? == "unreadable" {
                PreviewState::Unavailable
            } else {
                PreviewState::Pending
            };
            Ok((row.get::<_, i64>(0)?, state))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    let states = read().unwrap_or_default();
    files
        .iter()
        .map(|file| {
            states
                .get(&file.0)
                .copied()
                .unwrap_or(PreviewState::Pending)
        })
        .collect()
}

/// Each developed photograph's grid preview state at the entry it names (its current entry), in
/// the order of `photos`: `ready` when the index records a grid render of that entry, `pending`
/// otherwise, as [`grid_states`] reads files.
pub(super) fn photo_grid_states(
    index: &Connection,
    photos: &[(AssetId, Option<EntryId>)],
) -> Vec<PreviewState> {
    let read = || -> Result<HashSet<(String, String)>, crate::Error> {
        let mut statement = index.prepare_cached(
            "SELECT asset_id, entry_id FROM photo_previews
             WHERE tier = 'grid' AND asset_id IN (SELECT value FROM json_each(?1))",
        )?;
        let assets: Vec<&str> = photos.iter().map(|(asset, _)| asset.as_str()).collect();
        let rows =
            statement.query_map([json_list(&assets)?], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    };
    let ready = read().unwrap_or_default();
    photos
        .iter()
        .map(|(asset, entry)| match entry {
            Some(entry)
                if ready.contains(&(asset.as_str().to_owned(), entry.as_str().to_owned())) =>
            {
                PreviewState::Ready
            }
            _ => PreviewState::Pending,
        })
        .collect()
}
