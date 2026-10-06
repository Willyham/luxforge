//! The format-13 row writers and readers every path that touches these tables shares: volumes,
//! catalog folders, an asset's catalog columns and its capture row, collections and their members,
//! picks and indexed folders. The import, the seeder (`crate::seed`), the index lane and the
//! library write through these, so each table's SQL has one home.
//!
//! Each takes the caller's transaction ([`super::catalog::write`]) and validates nothing the schema
//! does not: the caller has already decided the change is allowed.
use super::AssetRecord;
use super::catalog::encode;
use crate::{
    Error,
    catalog_types::{
        AssetRowId, CaptureTime, CatalogFolder, CatalogFolderId, Collection, CollectionId,
        ExifOrientation, FileAvailability, HeaderMetadata, IndexedFolder, MomentId, Pick, Volume,
        VolumeId,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::path::Path;

/// Record a volume, or refresh what is known of it: its mount point, label, removability, platform
/// identifier and when it was last seen.
pub(crate) fn upsert_volume(tx: &Transaction<'_>, volume: &Volume) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO volumes (id, mount_point, label, removable, platform_id, last_seen_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET mount_point = excluded.mount_point,
             label = excluded.label, removable = excluded.removable,
             platform_id = excluded.platform_id, last_seen_ms = excluded.last_seen_ms",
        params![
            volume.id.as_str(),
            volume.mount_point.to_string_lossy(),
            volume.label,
            volume.removable,
            volume.platform_id,
            volume.last_seen_ms,
        ],
    )?;
    Ok(())
}

/// Insert one catalog folder; its counts and year are answers, not columns.
pub(crate) fn insert_catalog_folder(
    tx: &Transaction<'_>,
    folder: &CatalogFolder,
) -> Result<(), Error> {
    let event = folder.event.as_ref();
    tx.execute(
        "INSERT INTO catalog_folders (id, name, parent_id, created_ms, event_start_ms,
             event_end_ms, event_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            folder.id.as_str(),
            folder.name,
            folder.parent_id.as_ref().map(CatalogFolderId::as_str),
            folder.created_ms,
            event.map(|event| event.start_ms),
            event.map(|event| event.end_ms),
            event.and_then(|event| event.event_id.as_ref().map(|id| id.as_str().to_owned())),
        ],
    )?;
    Ok(())
}

/// Everything an asset row holds beside its record: where it lives in the catalog and on disk, when
/// it was developed, whether it is removed, and its original's availability.
pub(crate) struct NewAsset<'a> {
    pub record: &'a AssetRecord,
    /// The canonical path the record's locator resolves to, which no other asset may hold.
    pub canonical_locator: &'a str,
    pub catalog_folder_id: &'a CatalogFolderId,
    /// The folder on disk the photograph was developed from.
    pub source_folder: &'a Path,
    pub volume_id: &'a VolumeId,
    pub file_name: &'a str,
    pub developed_ms: i64,
    pub removed_ms: Option<i64>,
    pub availability: FileAvailability,
    pub checked_ms: i64,
    /// The burst or bracket the photograph was developed from, if any.
    pub develop_moment: Option<&'a MomentId>,
}

/// Insert one asset row and answer its [`AssetRowId`]. Its Original entry, state row and capture
/// row are the caller's to write in the same transaction.
pub(crate) fn insert_asset(
    tx: &Transaction<'_>,
    asset: &NewAsset<'_>,
) -> Result<AssetRowId, Error> {
    let record = asset.record;
    let byte_len = i64::try_from(record.byte_len)
        .map_err(|_| Error::resource_limit("source length exceeds catalog range"))?;
    tx.execute(
        "INSERT INTO assets (id, source_root, locator, canonical_locator, file_identity,
             fingerprint, byte_len, width, height, source_json, source_kind, catalog_folder_id,
             source_folder, volume_id, file_name, developed_ms, removed_ms, availability,
             checked_ms, develop_moment)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
             ?18, ?19, ?20)",
        params![
            record.id.as_str(),
            record.source_root.to_string_lossy(),
            record.locator.to_string_lossy(),
            asset.canonical_locator,
            record.file_identity,
            record.fingerprint,
            byte_len,
            i64::from(record.width),
            i64::from(record.height),
            encode(&record.source)?,
            record.source.tag().as_str(),
            asset.catalog_folder_id.as_str(),
            asset.source_folder.to_string_lossy(),
            asset.volume_id.as_str(),
            asset.file_name,
            asset.developed_ms,
            asset.removed_ms,
            asset.availability.as_str(),
            asset.checked_ms,
            asset.develop_moment.map(MomentId::as_str),
        ],
    )?;
    Ok(AssetRowId(tx.last_insert_rowid()))
}

/// Insert an asset's capture row from its header metadata and the place its position was named.
pub(crate) fn insert_capture(
    tx: &Transaction<'_>,
    row: AssetRowId,
    header: &HeaderMetadata,
    place: Option<&str>,
) -> Result<(), Error> {
    let capture = header.capture.as_ref();
    let position = header.position.as_ref();
    let camera = header.camera.as_ref();
    let exposure = &header.exposure;
    tx.execute(
        "INSERT INTO capture (asset_row, capture_ms, local_text, local_day, offset_minutes,
             latitude, longitude, altitude_m, place, make, model, body_serial, lens,
             exposure_time_s, f_number, iso, exposure_bias_ev, focal_mm, focal_35mm_mm, width,
             height, orientation)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
             ?18, ?19, ?20, ?21, ?22)",
        params![
            row.0,
            capture.map(CaptureTime::instant_ms),
            capture.map(|time| time.text.as_str()),
            capture.map(|time| time.local_day().to_string()),
            capture.and_then(|time| time.offset_minutes),
            position.map(|at| at.lat),
            position.map(|at| at.lon),
            position.and_then(|at| at.alt_m),
            place,
            camera.map(|body| body.make.as_str()),
            camera.map(|body| body.model.as_str()),
            camera.and_then(|body| body.serial.as_deref()),
            header.lens,
            exposure.time_s,
            exposure.f_number,
            exposure.iso,
            exposure.bias_ev,
            exposure.focal_mm,
            exposure.focal_35mm_mm,
            header.dimensions.map(|size| size.width),
            header.dimensions.map(|size| size.height),
            header.orientation.map(ExifOrientation::get),
        ],
    )?;
    Ok(())
}

/// An asset's capture row as header metadata and its place, or none when it has no row. The
/// thumbnail is the index's, never the catalog's, so it is always absent here.
#[cfg(test)]
pub(crate) fn capture_of(
    connection: &Connection,
    row: AssetRowId,
) -> Result<Option<(HeaderMetadata, Option<String>)>, Error> {
    use crate::catalog_types::{CameraBody, Dimensions, Exposure, GeoPosition};
    connection
        .query_row(
            "SELECT capture_ms, local_text, offset_minutes, latitude, longitude, altitude_m,
                 place, make, model, body_serial, lens, exposure_time_s, f_number, iso,
                 exposure_bias_ev, focal_mm, focal_35mm_mm, width, height, orientation
             FROM capture WHERE asset_row = ?1",
            [row.0],
            |row| {
                let instant: Option<i64> = row.get(0)?;
                let text: Option<String> = row.get(1)?;
                let offset: Option<i16> = row.get(2)?;
                let capture = instant.zip(text).map(|(instant, text)| CaptureTime {
                    local_ms: instant + i64::from(offset.unwrap_or(0)) * 60_000,
                    offset_minutes: offset,
                    text,
                });
                let position = row
                    .get::<_, Option<f64>>(3)?
                    .zip(row.get::<_, Option<f64>>(4)?)
                    .map(|(lat, lon)| GeoPosition {
                        lat,
                        lon,
                        alt_m: None,
                    })
                    .map(|position| -> rusqlite::Result<GeoPosition> {
                        Ok(GeoPosition {
                            alt_m: row.get(5)?,
                            ..position
                        })
                    })
                    .transpose()?;
                let camera = row
                    .get::<_, Option<String>>(7)?
                    .zip(row.get::<_, Option<String>>(8)?)
                    .map(|(make, model)| -> rusqlite::Result<CameraBody> {
                        Ok(CameraBody {
                            make,
                            model,
                            serial: row.get(9)?,
                        })
                    })
                    .transpose()?;
                let dimensions = row
                    .get::<_, Option<u32>>(17)?
                    .zip(row.get::<_, Option<u32>>(18)?)
                    .map(|(width, height)| Dimensions { width, height });
                Ok((
                    HeaderMetadata {
                        capture,
                        position,
                        camera,
                        lens: row.get(10)?,
                        exposure: Exposure {
                            time_s: row.get(11)?,
                            f_number: row.get(12)?,
                            iso: row.get(13)?,
                            bias_ev: row.get(14)?,
                            focal_mm: row.get(15)?,
                            focal_35mm_mm: row.get(16)?,
                        },
                        dimensions,
                        orientation: row.get::<_, Option<u8>>(19)?.and_then(ExifOrientation::new),
                        thumbnail: None,
                    },
                    row.get::<_, Option<String>>(6)?,
                ))
            },
        )
        .optional()
        .map_err(Error::from)
}

/// Insert one collection, smart collection or group; its count is an answer, not a column.
pub(crate) fn insert_collection(
    tx: &Transaction<'_>,
    collection: &Collection,
) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO collections (id, name, parent_id, kind, query_json, created_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            collection.id.as_str(),
            collection.name,
            collection.parent_id.as_ref().map(CollectionId::as_str),
            collection.kind.as_str(),
            collection.query.as_ref().map(encode).transpose()?,
            collection.created_ms,
        ],
    )?;
    Ok(())
}

/// Add one photograph to a plain collection.
pub(crate) fn insert_member(
    tx: &Transaction<'_>,
    collection: &CollectionId,
    asset: AssetRowId,
    added_ms: i64,
) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO collection_members (collection_id, asset_row, added_ms) VALUES (?1, ?2, ?3)",
        params![collection.as_str(), asset.0, added_ms],
    )?;
    Ok(())
}

/// Record one pick.
pub(crate) fn insert_pick(tx: &Transaction<'_>, pick: &Pick) -> Result<(), Error> {
    let identity = pick
        .signature
        .identity
        .map(|identity| identity.to_columns());
    tx.execute(
        "INSERT INTO picks (path, byte_len, modified_ns, device, inode, volume_id, actor,
             request_id, picked_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            pick.path.to_string_lossy(),
            i64::try_from(pick.signature.len)
                .map_err(|_| Error::resource_limit("file length exceeds catalog range"))?,
            pick.signature.modified_ns,
            identity.map(|(device, _)| device),
            identity.map(|(_, inode)| inode),
            pick.volume_id.as_str(),
            pick.actor,
            pick.request_id,
            pick.picked_ms,
        ],
    )?;
    Ok(())
}

/// Record one indexed folder.
pub(crate) fn insert_indexed_folder(
    tx: &Transaction<'_>,
    folder: &IndexedFolder,
) -> Result<(), Error> {
    tx.execute(
        "INSERT INTO indexed_folders (path, volume_id, added_ms, actor) VALUES (?1, ?2, ?3, ?4)",
        params![
            folder.path.to_string_lossy(),
            folder.volume_id.as_str(),
            folder.added_ms,
            folder.actor,
        ],
    )?;
    Ok(())
}

/// The indexed folders and the volumes the catalog knows, as the index lane's owner glue reads
/// them (`api/owner/files.rs`). One indexed folder is read by
/// [`library_rows::indexed_folder`](library_rows::indexed_folder), which the journal shares.
pub(crate) mod folder_rows {
    use super::*;

    /// Every indexed folder, in path order.
    pub(crate) fn indexed_folders(connection: &Connection) -> Result<Vec<IndexedFolder>, Error> {
        let mut statement = connection.prepare_cached(
            "SELECT path, volume_id, added_ms, actor FROM indexed_folders ORDER BY path",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (path, volume_id, added_ms, actor) = row?;
            Ok(IndexedFolder {
                path: path.into(),
                volume_id: VolumeId::parse(volume_id)?,
                added_ms,
                actor,
            })
        })
        .collect()
    }

    /// Every volume the catalog knows: the volumes its picks, photographs and indexed folders are
    /// on, as each was last seen.
    pub(crate) fn volumes(connection: &Connection) -> Result<Vec<Volume>, Error> {
        let mut statement = connection.prepare_cached(
            "SELECT id, mount_point, label, removable, platform_id, last_seen_ms FROM volumes
             ORDER BY label, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        rows.map(|row| {
            let (id, mount_point, label, removable, platform_id, last_seen_ms) = row?;
            Ok(Volume {
                id: VolumeId::parse(id)?,
                mount_point: mount_point.into(),
                label,
                removable,
                platform_id,
                last_seen_ms,
            })
        })
        .collect()
    }
}

/// The rows the library journal reads and writes one item at a time (`crate::library::items`).
/// Readers answer what is stored; writers answer SQLite's own result, with how many rows they
/// changed, so the journal can tell a refused value (a constraint) from a failure, and refuse an
/// item that is no longer there to change rather than record a change that did nothing.
pub(crate) mod library_rows {
    use super::super::catalog::decode;
    use super::*;
    use crate::{
        AssetId,
        catalog_types::{
            AssetSourceValue, CollectionKind, EventId, EventSpan, FileIdentity, FileSignature,
        },
    };

    /// A stored value the catalog holds but this build cannot read, as a row mapping's error.
    fn unreadable(column: usize) -> impl Fn(Error) -> rusqlite::Error {
        move |error| {
            rusqlite::Error::FromSqlConversionFailure(
                column,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        }
    }

    /// The columns [`read_pick`] maps, in its order.
    pub(crate) const PICK_COLUMNS: &str =
        "path, byte_len, modified_ns, device, inode, volume_id, actor, request_id, picked_ms";

    /// One pick from a row of [`PICK_COLUMNS`], without its index row.
    pub(crate) fn read_pick(row: &rusqlite::Row<'_>) -> rusqlite::Result<Pick> {
        Ok(Pick {
            path: row.get::<_, String>(0)?.into(),
            signature: FileSignature {
                len: row.get::<_, i64>(1)? as u64,
                modified_ns: row.get(2)?,
                identity: FileIdentity::from_columns(row.get(3)?, row.get(4)?),
            },
            volume_id: VolumeId::parse(row.get::<_, String>(5)?).map_err(unreadable(5))?,
            actor: row.get(6)?,
            request_id: row.get(7)?,
            picked_ms: row.get(8)?,
            file_id: None,
        })
    }

    /// The pick of the file at `path`, if it is picked.
    pub(crate) fn pick_at(connection: &Connection, path: &Path) -> Result<Option<Pick>, Error> {
        Ok(connection
            .prepare_cached(&format!("SELECT {PICK_COLUMNS} FROM picks WHERE path = ?1"))?
            .query_row([path.to_string_lossy()], read_pick)
            .optional()?)
    }

    /// Record a pick, replacing whatever pick its path had.
    pub(crate) fn put_pick(tx: &Transaction<'_>, pick: &Pick) -> rusqlite::Result<usize> {
        let identity = pick.signature.identity.map(FileIdentity::to_columns);
        tx.prepare_cached(
            "INSERT OR REPLACE INTO picks (path, byte_len, modified_ns, device, inode, volume_id,
                 actor, request_id, picked_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?
        .execute(params![
            pick.path.to_string_lossy(),
            pick.signature.len as i64,
            pick.signature.modified_ns,
            identity.map(|(device, _)| device),
            identity.map(|(_, inode)| inode),
            pick.volume_id.as_str(),
            pick.actor,
            pick.request_id,
            pick.picked_ms,
        ])
    }

    /// Clear the pick of the file at `path`.
    pub(crate) fn delete_pick(tx: &Transaction<'_>, path: &Path) -> rusqlite::Result<usize> {
        tx.prepare_cached("DELETE FROM picks WHERE path = ?1")?
            .execute([path.to_string_lossy()])
    }

    /// The catalog folder a photograph is in, or none when the catalog has no such photograph.
    pub(crate) fn asset_folder(
        connection: &Connection,
        asset: &AssetId,
    ) -> Result<Option<CatalogFolderId>, Error> {
        connection
            .prepare_cached("SELECT catalog_folder_id FROM assets WHERE id = ?1")?
            .query_row([asset.as_str()], |row| row.get::<_, String>(0))
            .optional()?
            .map(CatalogFolderId::parse)
            .transpose()
    }

    pub(crate) fn set_asset_folder(
        tx: &Transaction<'_>,
        asset: &AssetId,
        folder: &CatalogFolderId,
    ) -> rusqlite::Result<usize> {
        tx.prepare_cached("UPDATE assets SET catalog_folder_id = ?1 WHERE id = ?2")?
            .execute(params![folder.as_str(), asset.as_str()])
    }

    /// When a photograph was removed (`Some(None)` while it is not), or none when the catalog has
    /// no such photograph.
    pub(crate) fn asset_removed(
        connection: &Connection,
        asset: &AssetId,
    ) -> Result<Option<Option<i64>>, Error> {
        Ok(connection
            .prepare_cached("SELECT removed_ms FROM assets WHERE id = ?1")?
            .query_row([asset.as_str()], |row| row.get::<_, Option<i64>>(0))
            .optional()?)
    }

    pub(crate) fn set_asset_removed(
        tx: &Transaction<'_>,
        asset: &AssetId,
        removed_ms: Option<i64>,
    ) -> rusqlite::Result<usize> {
        tx.prepare_cached("UPDATE assets SET removed_ms = ?1 WHERE id = ?2")?
            .execute(params![removed_ms, asset.as_str()])
    }

    /// Where the catalog looks for a photograph's original, or none when it has no such
    /// photograph.
    pub(crate) fn asset_source(
        connection: &Connection,
        asset: &AssetId,
    ) -> Result<Option<AssetSourceValue>, Error> {
        Ok(connection
            .prepare_cached(
                "SELECT locator, source_folder, volume_id, file_identity FROM assets
                 WHERE id = ?1",
            )?
            .query_row([asset.as_str()], |row| {
                Ok(AssetSourceValue {
                    locator: row.get::<_, String>(0)?.into(),
                    source_folder: row.get::<_, String>(1)?.into(),
                    volume_id: VolumeId::parse(row.get::<_, String>(2)?).map_err(unreadable(2))?,
                    file_identity: row.get(3)?,
                })
            })
            .optional()?)
    }

    /// Point a photograph at the original `source` names: its locator (which is also its
    /// canonical locator), source root and folder, file name, volume and file identity, as a
    /// relocation writes them.
    pub(crate) fn set_asset_source(
        tx: &Transaction<'_>,
        asset: &AssetId,
        source: &AssetSourceValue,
    ) -> rusqlite::Result<usize> {
        let file_name = source
            .locator
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        tx.prepare_cached(
            "UPDATE assets SET locator = ?1, canonical_locator = ?1, source_root = ?2,
                 source_folder = ?2, file_name = ?3, volume_id = ?4, file_identity = ?5
             WHERE id = ?6",
        )?
        .execute(params![
            source.locator.to_string_lossy(),
            source.source_folder.to_string_lossy(),
            file_name,
            source.volume_id.as_str(),
            source.file_identity,
            asset.as_str(),
        ])
    }

    /// What keeps a photograph from being sent back, as its refusal says it: history beyond its
    /// Original entry, a named version, or a collection it is in; none for a photograph that holds
    /// only its Original. Three indexed counts.
    pub(crate) fn kept_by(
        connection: &Connection,
        asset: &AssetId,
    ) -> Result<Option<&'static str>, Error> {
        let (entries, versions, memberships): (i64, i64, i64) = connection
            .prepare_cached(
                "SELECT (SELECT count(*) FROM entries WHERE asset_id = ?1),
                     (SELECT count(*) FROM versions WHERE asset_id = ?1),
                     (SELECT count(*) FROM collection_members m
                         JOIN assets a ON a.row_id = m.asset_row WHERE a.id = ?1)",
            )?
            .query_row([asset.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        Ok(if entries > 1 {
            Some("has been edited since it was developed")
        } else if versions > 0 {
            Some("has a named version")
        } else if memberships > 0 {
            Some("is in a collection")
        } else {
            None
        })
    }

    /// Delete a photograph's catalog record — its request log, versions, memberships, capture
    /// row, state row, entries and asset row — answering how many asset rows went (none when the
    /// catalog has no such photograph). The caller has checked it holds nothing but its Original
    /// ([`kept_by`]); nothing on disk is touched, and the journal keeps what it recorded of it.
    pub(crate) fn delete_photograph(
        tx: &Transaction<'_>,
        asset: &AssetId,
    ) -> rusqlite::Result<usize> {
        let id = asset.as_str();
        for table in ["requests", "versions"] {
            tx.prepare_cached(&format!("DELETE FROM {table} WHERE asset_id = ?1"))?
                .execute([id])?;
        }
        for table in ["collection_members", "capture"] {
            tx.prepare_cached(&format!(
                "DELETE FROM {table} WHERE asset_row = (SELECT row_id FROM assets WHERE id = ?1)"
            ))?
            .execute([id])?;
        }
        for table in ["asset_state", "entries"] {
            tx.prepare_cached(&format!("DELETE FROM {table} WHERE asset_id = ?1"))?
                .execute([id])?;
        }
        tx.prepare_cached("DELETE FROM assets WHERE id = ?1")?
            .execute([id])
    }

    /// Delete the whole catalog record of each of `assets` — its entries' artifact references,
    /// versions, state row, request log, collapsed entries, collection memberships, capture row,
    /// entries and asset row — one statement per table for them all, answering how many asset rows
    /// went. Only emptying Removed calls it, with the artifact references' and collapsed entries'
    /// permanence lifted for it (`crate::library::remove::empty`); nothing on disk is touched, and the journal keeps what it
    /// recorded of them.
    pub(crate) fn delete_photographs(
        tx: &Transaction<'_>,
        assets: &[AssetId],
    ) -> Result<usize, Error> {
        let ids = serde_json::to_string(assets)
            .map_err(|error| Error::internal(format!("cannot encode photographs: {error}")))?;
        const THESE: &str = "(SELECT value FROM json_each(?1))";
        // The entries' references and their collapsed marks, both keyed by entry.
        for table in ["artifact_refs", "collapsed_entries"] {
            tx.prepare_cached(&format!(
                "DELETE FROM {table}
                 WHERE entry_id IN (SELECT id FROM entries WHERE asset_id IN {THESE})"
            ))?
            .execute([&ids])?;
        }
        for table in ["versions", "asset_state", "requests"] {
            tx.prepare_cached(&format!("DELETE FROM {table} WHERE asset_id IN {THESE}"))?
                .execute([&ids])?;
        }
        for table in ["collection_members", "capture"] {
            tx.prepare_cached(&format!(
                "DELETE FROM {table} WHERE asset_row IN (SELECT row_id FROM assets WHERE id IN {THESE})"
            ))?
            .execute([&ids])?;
        }
        tx.prepare_cached(&format!("DELETE FROM entries WHERE asset_id IN {THESE}"))?
            .execute([&ids])?;
        Ok(tx
            .prepare_cached(&format!("DELETE FROM assets WHERE id IN {THESE}"))?
            .execute([&ids])?)
    }

    /// Whether the catalog holds the photograph `asset`.
    pub(crate) fn has_asset(connection: &Connection, asset: &AssetId) -> Result<bool, Error> {
        Ok(connection
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM assets WHERE id = ?1)")?
            .query_row([asset.as_str()], |row| row.get(0))?)
    }

    /// The columns [`read_catalog_folder`] maps, in its order.
    pub(crate) const CATALOG_FOLDER_COLUMNS: &str =
        "id, name, parent_id, created_ms, event_start_ms, event_end_ms, event_key";

    /// One catalog folder from a row of [`CATALOG_FOLDER_COLUMNS`], its counts left at zero.
    pub(crate) fn read_catalog_folder(row: &rusqlite::Row<'_>) -> rusqlite::Result<CatalogFolder> {
        let start: Option<i64> = row.get(4)?;
        let end: Option<i64> = row.get(5)?;
        let key: Option<String> = row.get(6)?;
        let event_id = key.map(EventId::parse).transpose().map_err(unreadable(6))?;
        Ok(CatalogFolder {
            id: CatalogFolderId::parse(row.get::<_, String>(0)?).map_err(unreadable(0))?,
            name: row.get(1)?,
            parent_id: row
                .get::<_, Option<String>>(2)?
                .map(CatalogFolderId::parse)
                .transpose()
                .map_err(unreadable(2))?,
            created_ms: row.get(3)?,
            event: start.zip(end).map(|(start_ms, end_ms)| EventSpan {
                start_ms,
                end_ms,
                event_id,
            }),
            count: 0,
            year: None,
        })
    }

    pub(crate) fn catalog_folder(
        connection: &Connection,
        id: &CatalogFolderId,
    ) -> Result<Option<CatalogFolder>, Error> {
        Ok(connection
            .prepare_cached(&format!(
                "SELECT {CATALOG_FOLDER_COLUMNS} FROM catalog_folders WHERE id = ?1"
            ))?
            .query_row([id.as_str()], read_catalog_folder)
            .optional()?)
    }

    /// Record a catalog folder, or rewrite the one with its identity.
    pub(crate) fn put_catalog_folder(
        tx: &Transaction<'_>,
        folder: &CatalogFolder,
    ) -> rusqlite::Result<usize> {
        let event = folder.event.as_ref();
        tx.prepare_cached(
            "INSERT INTO catalog_folders (id, name, parent_id, created_ms, event_start_ms,
                 event_end_ms, event_key)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, parent_id = excluded.parent_id,
                 created_ms = excluded.created_ms, event_start_ms = excluded.event_start_ms,
                 event_end_ms = excluded.event_end_ms, event_key = excluded.event_key",
        )?
        .execute(params![
            folder.id.as_str(),
            folder.name,
            folder.parent_id.as_ref().map(CatalogFolderId::as_str),
            folder.created_ms,
            event.map(|event| event.start_ms),
            event.map(|event| event.end_ms),
            event.and_then(|event| event.event_id.as_ref().map(|id| id.as_str().to_owned())),
        ])
    }

    pub(crate) fn delete_catalog_folder(
        tx: &Transaction<'_>,
        id: &CatalogFolderId,
    ) -> rusqlite::Result<usize> {
        tx.prepare_cached("DELETE FROM catalog_folders WHERE id = ?1")?
            .execute([id.as_str()])
    }

    /// The columns [`read_collection`] maps, in its order.
    pub(crate) const COLLECTION_COLUMNS: &str = "id, name, parent_id, kind, query_json, created_ms";

    /// One collection from a row of [`COLLECTION_COLUMNS`], its count left unknown.
    pub(crate) fn read_collection(row: &rusqlite::Row<'_>) -> rusqlite::Result<Collection> {
        let kind: String = row.get(3)?;
        Ok(Collection {
            id: CollectionId::parse(row.get::<_, String>(0)?).map_err(unreadable(0))?,
            name: row.get(1)?,
            parent_id: row
                .get::<_, Option<String>>(2)?
                .map(CollectionId::parse)
                .transpose()
                .map_err(unreadable(2))?,
            kind: CollectionKind::parse(&kind).ok_or_else(|| {
                unreadable(3)(Error::incompatible(format!(
                    "collection kind {kind} is not supported"
                )))
            })?,
            query: row
                .get::<_, Option<String>>(4)?
                .map(|query| decode("stored collection query", query))
                .transpose()
                .map_err(unreadable(4))?,
            created_ms: row.get(5)?,
            count: None,
        })
    }

    pub(crate) fn collection(
        connection: &Connection,
        id: &CollectionId,
    ) -> Result<Option<Collection>, Error> {
        Ok(connection
            .prepare_cached(&format!(
                "SELECT {COLLECTION_COLUMNS} FROM collections WHERE id = ?1"
            ))?
            .query_row([id.as_str()], read_collection)
            .optional()?)
    }

    /// Record a collection, or rewrite the one with its identity (never its kind, which the
    /// schema keeps).
    pub(crate) fn put_collection(
        tx: &Transaction<'_>,
        collection: &Collection,
    ) -> rusqlite::Result<usize> {
        let query = collection
            .query
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        tx.prepare_cached(
            "INSERT INTO collections (id, name, parent_id, kind, query_json, created_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET name = excluded.name, parent_id = excluded.parent_id,
                 kind = excluded.kind, query_json = excluded.query_json,
                 created_ms = excluded.created_ms",
        )?
        .execute(params![
            collection.id.as_str(),
            collection.name,
            collection.parent_id.as_ref().map(CollectionId::as_str),
            collection.kind.as_str(),
            query,
            collection.created_ms,
        ])
    }

    pub(crate) fn delete_collection(
        tx: &Transaction<'_>,
        id: &CollectionId,
    ) -> rusqlite::Result<usize> {
        tx.prepare_cached("DELETE FROM collections WHERE id = ?1")?
            .execute([id.as_str()])
    }

    /// When a photograph joined a collection, or none when it is not a member.
    pub(crate) fn membership(
        connection: &Connection,
        collection: &CollectionId,
        asset: &AssetId,
    ) -> Result<Option<i64>, Error> {
        Ok(connection
            .prepare_cached(
                "SELECT m.added_ms FROM collection_members m
                 JOIN assets a ON a.row_id = m.asset_row
                 WHERE m.collection_id = ?1 AND a.id = ?2",
            )?
            .query_row([collection.as_str(), asset.as_str()], |row| row.get(0))
            .optional()?)
    }

    /// Make a photograph a member of a collection since `added_ms`; 0 when there is no such
    /// photograph.
    pub(crate) fn put_membership(
        tx: &Transaction<'_>,
        collection: &CollectionId,
        asset: &AssetId,
        added_ms: i64,
    ) -> rusqlite::Result<usize> {
        tx.prepare_cached(
            "INSERT OR REPLACE INTO collection_members (collection_id, asset_row, added_ms)
             SELECT ?1, row_id, ?3 FROM assets WHERE id = ?2",
        )?
        .execute(params![collection.as_str(), asset.as_str(), added_ms])
    }

    pub(crate) fn delete_membership(
        tx: &Transaction<'_>,
        collection: &CollectionId,
        asset: &AssetId,
    ) -> rusqlite::Result<usize> {
        tx.prepare_cached(
            "DELETE FROM collection_members
             WHERE collection_id = ?1 AND asset_row = (SELECT row_id FROM assets WHERE id = ?2)",
        )?
        .execute([collection.as_str(), asset.as_str()])
    }

    /// The indexed folder at `path`, if there is one.
    pub(crate) fn indexed_folder(
        connection: &Connection,
        path: &Path,
    ) -> Result<Option<IndexedFolder>, Error> {
        Ok(connection
            .prepare_cached(
                "SELECT path, volume_id, added_ms, actor FROM indexed_folders WHERE path = ?1",
            )?
            .query_row([path.to_string_lossy()], |row| {
                Ok(IndexedFolder {
                    path: row.get::<_, String>(0)?.into(),
                    volume_id: VolumeId::parse(row.get::<_, String>(1)?).map_err(unreadable(1))?,
                    added_ms: row.get(2)?,
                    actor: row.get(3)?,
                })
            })
            .optional()?)
    }

    /// Record an indexed folder, replacing the one at its path.
    pub(crate) fn put_indexed_folder(
        tx: &Transaction<'_>,
        folder: &IndexedFolder,
    ) -> rusqlite::Result<usize> {
        tx.prepare_cached(
            "INSERT OR REPLACE INTO indexed_folders (path, volume_id, added_ms, actor)
             VALUES (?1, ?2, ?3, ?4)",
        )?
        .execute(params![
            folder.path.to_string_lossy(),
            folder.volume_id.as_str(),
            folder.added_ms,
            folder.actor,
        ])
    }

    pub(crate) fn delete_indexed_folder(
        tx: &Transaction<'_>,
        path: &Path,
    ) -> rusqlite::Result<usize> {
        tx.prepare_cached("DELETE FROM indexed_folders WHERE path = ?1")?
            .execute([path.to_string_lossy()])
    }

    /// Whether the catalog records the volume `id`.
    pub(crate) fn has_volume(connection: &Connection, id: &VolumeId) -> Result<bool, Error> {
        Ok(connection
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM volumes WHERE id = ?1)")?
            .query_row([id.as_str()], |row| row.get(0))?)
    }
}
