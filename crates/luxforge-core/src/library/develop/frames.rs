//! What organizing reads of files, from the index: the [`FrameFacts`] of every file the index lists
//! in some folders, and of files it does not list, as `crate::organize`'s events and moments take
//! them. A Develop plans with it; it is written for any caller that organizes files it names by
//! folder (the views lane's events and grouping read the same columns), and reads nothing but the
//! index's rows.
use crate::{
    Error,
    catalog_types::{
        CameraBody, Exposure, FileId, FrameFacts, FrameTables, GeoPosition, LocalDay, ViewItem,
    },
};
use rusqlite::Connection;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

const DAY_MS: i64 = 86_400_000;

/// Frames and the folder and body tables they name.
#[derive(Debug, Default)]
pub(crate) struct Frames {
    pub frames: Vec<FrameFacts>,
    pub tables: FrameTables,
}

impl Frames {
    /// The path of `frame`: its folder joined with its name.
    pub(crate) fn path(&self, frame: &FrameFacts) -> PathBuf {
        self.tables.folder_path(frame.folder).join(&*frame.name)
    }
}

/// The stand-in item of the `index`th file the index does not list: a negative file row, which no
/// index row has, so it can be told apart from every listed file.
pub(crate) fn unlisted_item(index: usize) -> ViewItem {
    ViewItem::File(FileId(-1 - index as i64))
}

/// Every file the index lists directly in each of `folders` (not in their subfolders), each once,
/// as a frame whose item is its index row; then each of `unlisted`, files the index does not list,
/// as an undated frame of its folder whose item is [`unlisted_item`] of its position. One indexed
/// query per folder (`files_by_folder`); a folder's rows cost one frame each and one string, the
/// name.
pub(crate) fn frames(
    index: &Connection,
    folders: &[PathBuf],
    unlisted: &[PathBuf],
) -> Result<Frames, Error> {
    let mut read = Frames::default();
    let mut statement = index.prepare_cached(
        "SELECT id, name, capture_ms, offset_minutes, latitude, longitude, altitude_m, make, model,
             body_serial, exposure_time_s, f_number, iso, exposure_bias_ev, focal_mm,
             focal_35mm_mm
         FROM files WHERE folder = ?1",
    )?;
    let mut seen = HashSet::new();
    for folder in folders {
        if !seen.insert(folder) {
            continue;
        }
        let index = read.tables.folder(folder.clone());
        let mut rows = statement.query([folder.to_string_lossy()])?;
        while let Some(row) = rows.next()? {
            let instant: Option<i64> = row.get(2)?;
            let offset: Option<i64> = row.get(3)?;
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
            let position = row
                .get::<_, Option<f64>>(4)?
                .zip(row.get::<_, Option<f64>>(5)?)
                .and_then(|(lat, lon)| GeoPosition::new(lat, lon, None));
            let position = match position {
                Some(position) => Some(GeoPosition {
                    alt_m: row.get::<_, Option<f64>>(6)?.filter(|alt| alt.is_finite()),
                    ..position
                }),
                None => None,
            };
            read.frames.push(FrameFacts {
                item: ViewItem::File(FileId(row.get(0)?)),
                folder: index,
                name: row.get::<_, String>(1)?.into(),
                instant_ms: instant,
                // The camera's own day: its wall clock, the instant moved back by its offset.
                local_day: instant.map(|instant| {
                    LocalDay((instant + offset.unwrap_or(0) * 60_000).div_euclid(DAY_MS) as i32)
                }),
                position,
                body: read.tables.body(camera.as_ref()),
                exposure: Exposure {
                    time_s: row.get(10)?,
                    f_number: row.get(11)?,
                    iso: row.get(12)?,
                    bias_ev: row.get(13)?,
                    focal_mm: row.get(14)?,
                    focal_35mm_mm: row.get(15)?,
                },
            });
        }
    }
    for (at, path) in unlisted.iter().enumerate() {
        let folder = read
            .tables
            .folder(path.parent().unwrap_or(Path::new("")).to_path_buf());
        read.frames.push(FrameFacts {
            item: unlisted_item(at),
            folder,
            name: path
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned())
                .into(),
            instant_ms: None,
            local_day: None,
            position: None,
            body: read.tables.body(None),
            exposure: Exposure::default(),
        });
    }
    Ok(read)
}
