//! The format-12 row writers and readers every path that touches these tables shares: volumes,
//! catalog folders, an asset's catalog columns and its capture row, collections and their members,
//! picks and indexed folders. The import, the seeder (`crate::seed`) and the lanes write through
//! these, so each table's SQL has one home; a lane adds the writer it needs here beside the others.
//!
//! Each takes the caller's transaction ([`super::catalog::write`]) and validates nothing the schema
//! does not: the caller has already decided the change is allowed.
use super::AssetRecord;
use super::catalog::encode;
use crate::{
    Error,
    catalog_types::{
        AssetRowId, CameraBody, CaptureTime, CatalogFolder, CatalogFolderId, Collection,
        CollectionId, Dimensions, ExifOrientation, Exposure, FileAvailability, GeoPosition,
        HeaderMetadata, IndexedFolder, MomentId, Pick, Volume, VolumeId,
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

/// The top-level catalog folder named `name` (ignoring case), made now when there is none: where
/// the single-file import puts a photograph until picks are developed into chosen folders.
pub(crate) fn top_level_folder(
    tx: &Transaction<'_>,
    name: &str,
    now_ms: i64,
) -> Result<CatalogFolderId, Error> {
    let existing: Option<String> = tx
        .query_row(
            "SELECT id FROM catalog_folders WHERE parent_id IS NULL AND name = ?1 COLLATE NOCASE",
            [name],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return CatalogFolderId::parse(id);
    }
    let folder = CatalogFolder {
        id: CatalogFolderId::new(),
        name: name.to_owned(),
        parent_id: None,
        created_ms: now_ms,
        event: None,
        count: 0,
        year: None,
    };
    insert_catalog_folder(tx, &folder)?;
    Ok(folder.id)
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
#[allow(dead_code, reason = "catalog contracts: used as the lanes land")]
pub(crate) fn capture_of(
    connection: &Connection,
    row: AssetRowId,
) -> Result<Option<(HeaderMetadata, Option<String>)>, Error> {
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
