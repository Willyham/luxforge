//! What the grid can draw for a row now ([`PreviewState`]). A row's grid state is the preview
//! lane's answer for files and developed photographs alike (`PreviewsLane::view_grid_states`: its
//! cache's valid records, the renders of each photograph's current entry, and the failures it
//! remembers), which the owner glue lends to [`super::rows`] as a [`GridStates`]; the tests lend
//! [`index_grid_states`], which reads the index's records alone.

use crate::{
    Error,
    catalog_types::{PreviewState, ViewItem},
};

/// Where a window's rows read their grid preview states: each item's, in the order of the items.
/// It is called with no borrow of the index held.
pub(crate) type GridStates<'a> = &'a dyn Fn(&[ViewItem]) -> Result<Vec<PreviewState>, Error>;

#[cfg(test)]
pub(crate) use index::index_grid_states;

/// The tests' provider: the index's records alone, as the model the tests compare with reads them.
#[cfg(test)]
mod index {
    use super::super::json_list;
    use crate::{
        AssetId, EntryId, Error,
        catalog_types::{FileId, PreviewState, ViewItem},
    };
    use rusqlite::Connection;
    use std::collections::HashSet;

    /// Each item's grid state from the index's records alone: [`grid_states`] for files, and for a
    /// photograph `ready` when the index records a grid render of its current entry. The tests'
    /// provider, which the model they compare with reads the same way.
    pub(crate) fn index_grid_states(
        service: &crate::EditorService,
        items: &[ViewItem],
    ) -> Result<Vec<PreviewState>, Error> {
        let files: Vec<FileId> = items
            .iter()
            .filter_map(|item| match item {
                ViewItem::File(file) => Some(*file),
                ViewItem::Photo(_) => None,
            })
            .collect();
        let rows: Vec<i64> = items
            .iter()
            .filter_map(|item| match item {
                ViewItem::Photo(row) => Some(row.0),
                ViewItem::File(_) => None,
            })
            .collect();
        let mut entries: std::collections::HashMap<i64, (AssetId, Option<EntryId>)> =
            std::collections::HashMap::new();
        if !rows.is_empty() {
            let mut statement = service.connection.prepare_cached(
                "SELECT a.row_id, a.id, s.current_entry_id FROM assets a
                     LEFT JOIN asset_state s ON s.asset_id = a.id
                 WHERE a.row_id IN (SELECT value FROM json_each(?1))",
            )?;
            let found = statement.query_map([json_list(&rows)?], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;
            for found in found {
                let (row, asset, entry) = found?;
                entries.insert(
                    row,
                    (
                        AssetId::parse(asset)?,
                        entry.map(EntryId::parse).transpose()?,
                    ),
                );
            }
        }
        let index = service.index()?;
        let mut file_states = grid_states(index.connection(), &files)?.into_iter();
        let wanted: Vec<(AssetId, Option<EntryId>)> = rows
            .iter()
            .filter_map(|row| entries.get(row).cloned())
            .collect();
        let mut photo_states = photo_grid_states(index.connection(), &wanted).into_iter();
        Ok(items
            .iter()
            .map(|item| match item {
                ViewItem::File(_) => file_states.next(),
                ViewItem::Photo(_) => photo_states.next(),
            })
            .map(|state| state.unwrap_or(PreviewState::Pending))
            .collect())
    }

    /// Each file's grid preview state from the index's records alone, in the order of `files`: `ready`
    /// when the index records its grid tier, `thumbnail` when its header records an embedded
    /// thumbnail, `unavailable` when its header could not be read, and `pending` otherwise; a file the
    /// index no longer has, or a read that fails, is `pending`. The tests' provider, which the model
    /// they compare with reads the same way.
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
}
