//! `catalog.info` on the owner: the catalog's path, identity and formats, the counts behind the
//! Catalog sources, and the index's size. It reads counts and file sizes only, and opens no index
//! that does not exist yet.
use super::{Call, Owner};
use crate::{
    EditorService, Error, INDEX_FILE, INDEX_FORMAT,
    api::{methods::value, params::NoParams},
    catalog_types::{CacheSize, CatalogCounts, CatalogInfo, DEFAULT_RECENT_DAYS},
    editor::{CATALOG_FORMAT, now_ms},
};
use serde_json::Value;
use std::path::PathBuf;

/// `catalog.info`.
pub(in crate::api) fn catalog_info(
    owner: &mut Owner,
    _: &Call<'_>,
    _: NoParams,
) -> Result<Value, Error> {
    let service = &owner.service;
    value(CatalogInfo {
        path: service
            .connection
            .path()
            .map(PathBuf::from)
            .unwrap_or_default(),
        catalog_id: service.catalog_id().to_owned(),
        format: CATALOG_FORMAT,
        index_format: INDEX_FORMAT,
        counts: counts(service)?,
        index: index_size(service)?,
        previews: None,
    })
}

fn counts(service: &EditorService) -> Result<CatalogCounts, Error> {
    let recent_since = now_ms() - i64::from(DEFAULT_RECENT_DAYS) * 24 * 60 * 60 * 1000;
    Ok(service
        .connection
        .prepare_cached(
            "SELECT
             (SELECT count(*) FROM assets WHERE removed_ms IS NULL),
             (SELECT count(*) FROM assets WHERE removed_ms IS NULL AND developed_ms >= ?1),
             (SELECT count(*) FROM assets WHERE removed_ms IS NOT NULL),
             (SELECT count(*) FROM assets
                 WHERE removed_ms IS NULL AND availability <> 'available'),
             (SELECT count(*) FROM catalog_folders),
             (SELECT count(*) FROM collections),
             (SELECT count(*) FROM picks),
             (SELECT count(*) FROM indexed_folders),
             (SELECT count(*) FROM library_changes)",
        )?
        .query_row([recent_since], |row| {
            let count = |index| row.get::<_, i64>(index).map(|count| count as u64);
            Ok(CatalogCounts {
                photographs: count(0)?,
                recently_developed: count(1)?,
                removed: count(2)?,
                unavailable: count(3)?,
                folders: count(4)?,
                collections: count(5)?,
                picks: count(6)?,
                indexed_folders: count(7)?,
                library_changes: count(8)?,
            })
        })?)
}

/// The index database's bytes on disk, with its write-ahead log, and the files it lists; zero
/// before it is first used.
fn index_size(service: &EditorService) -> Result<CacheSize, Error> {
    let path = service.index_dir().join(INDEX_FILE);
    if !path.exists() {
        return Ok(CacheSize {
            path,
            bytes: 0,
            files: 0,
        });
    }
    let bytes = ["", "-wal", "-shm"]
        .iter()
        .filter_map(|suffix| {
            let mut name = path.clone().into_os_string();
            name.push(suffix);
            std::fs::metadata(name).ok()
        })
        .map(|metadata| metadata.len())
        .sum();
    let index = service.index()?;
    let files: i64 = index
        .connection()
        .prepare_cached("SELECT count(*) FROM files")?
        .query_row([], |row| row.get(0))?;
    Ok(CacheSize {
        path,
        bytes,
        files: files as u64,
    })
}
