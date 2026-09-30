//! What the grid can draw for a row now ([`PreviewState`]). A file's grid state is the preview
//! lane's answer (`PreviewsLane::grid_states`: its cache's valid records and the failures it
//! remembers), which the owner glue lends to [`super::rows`] as a [`GridStates`]; the tests lend
//! [`grid_states`], which reads the index's records alone. A developed photograph's is read here
//! until the lane renders them.

use super::json_list;
use crate::{
    AssetId, EntryId, Error,
    catalog_types::{FileId, PreviewState},
};
use rusqlite::Connection;
use std::collections::HashSet;

/// Where the rows of a view over files read their grid preview states: each file's state, in the
/// order of the files, from the index connection given.
pub(crate) type GridStates<'a> =
    &'a dyn Fn(&Connection, &[FileId]) -> Result<Vec<PreviewState>, Error>;

/// Each file's grid preview state from the index's records alone, in the order of `files`: `ready`
/// when the index records its grid tier, `thumbnail` when its header records an embedded
/// thumbnail, `unavailable` when its header could not be read, and `pending` otherwise; a file the
/// index no longer has, or a read that fails, is `pending`. The tests' provider, which the model
/// they compare with reads the same way.
#[cfg(test)]
pub(crate) fn grid_states(
    index: &Connection,
    files: &[FileId],
) -> Result<Vec<PreviewState>, Error> {
    let read = || -> Result<std::collections::HashMap<i64, PreviewState>, crate::Error> {
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
    Ok(files
        .iter()
        .map(|file| {
            states
                .get(&file.0)
                .copied()
                .unwrap_or(PreviewState::Pending)
        })
        .collect())
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
