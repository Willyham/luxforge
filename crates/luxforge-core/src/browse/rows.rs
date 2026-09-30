//! Windows of a view by position (`browse.rows`): the window's items come from the held list with
//! no query, and their rows are read in a fixed number of statements whatever the window's size —
//! for files the index's rows and preview records, the offline roots, and the catalog's picks and
//! developed originals among the window's paths; for photographs the catalog's rows and the
//! index's rendered previews.

use super::{
    View,
    candidates::{exposure, position},
    json_list, places,
    previews::{GridStates, photo_grid_states},
    within,
};
use crate::{
    AssetId, EditorService, EntryId, Error, SourceTag,
    catalog_types::{
        CameraBody, Dimensions, ExifOrientation, Exposure, FileAvailability, FileId, Moment,
        MomentRef, PreviewState, RowItem, ViewItem, ViewRow,
    },
};
use rusqlite::Row;
use std::collections::HashMap;

/// Rows `from..from + count` of `view`, fewer at its end. Refused with `validation` when `from` is
/// past the end, and with `conflict` when an item of the window is no longer in the index or the
/// catalog, which only a stale view can hold.
pub(crate) fn rows(
    service: &EditorService,
    view: &View,
    from: u32,
    count: u32,
    grid: GridStates<'_>,
) -> Result<Vec<ViewRow>, Error> {
    let start = from as usize;
    if start > view.items.len() {
        return Err(Error::validation(format!(
            "row {from} is past the view's {} items",
            view.items.len()
        )));
    }
    let end = start.saturating_add(count as usize).min(view.items.len());
    let window = &view.items[start..end];
    if view.over_files {
        file_rows(service, window, from, &view.layout.moments, grid)
    } else {
        photo_rows(service, window, from, &view.layout.moments)
    }
}

/// Where `position` sits in its moment, if one covers it. Moments are disjoint and in view order.
pub(super) fn moment_of(moments: &[Moment], position: u32) -> Option<MomentRef> {
    let at = moments.partition_point(|moment| moment.start + moment.len <= position);
    let moment = moments.get(at)?;
    (moment.start <= position).then(|| MomentRef {
        index: at as u32,
        frame: position - moment.start,
    })
}

fn gone(position: u32) -> Error {
    Error::conflict(format!(
        "the view is stale: its item at {position} is gone; evaluate it again"
    ))
}

/// What one `files` row gives a view row.
struct FileFacts {
    path: String,
    name: String,
    kind: SourceTag,
    dimensions: Option<Dimensions>,
    orientation: Option<ExifOrientation>,
    capture: Option<String>,
    place: Option<String>,
    camera: Option<String>,
    lens: Option<String>,
    exposure: Exposure,
    folder: String,
}

fn camera_label(row: &Row<'_>, first: usize) -> Result<Option<String>, Error> {
    let make: Option<String> = row.get(first)?;
    let model: Option<String> = row.get(first + 1)?;
    Ok(make.zip(model).map(|(make, model)| {
        CameraBody {
            make,
            model,
            serial: None,
        }
        .label()
    }))
}

fn dimensions(row: &Row<'_>, first: usize) -> Result<Option<Dimensions>, Error> {
    let width: Option<u32> = row.get(first)?;
    let height: Option<u32> = row.get(first + 1)?;
    Ok(width
        .zip(height)
        .map(|(width, height)| Dimensions { width, height }))
}

fn file_rows(
    service: &EditorService,
    window: &[ViewItem],
    from: u32,
    moments: &[Moment],
    grid: GridStates<'_>,
) -> Result<Vec<ViewRow>, Error> {
    let ids: Vec<FileId> = window
        .iter()
        .map(|item| match item {
            ViewItem::File(file) => *file,
            ViewItem::Photo(_) => unreachable!("a view over files holds files"),
        })
        .collect();
    let index = service.index()?;
    let connection = index.connection();
    let mut statement = connection.prepare_cached(
        "SELECT id, path, name, kind, width, height, orientation, local_text, latitude, longitude,
             altitude_m, make, model, lens, exposure_time_s, f_number, iso, exposure_bias_ev,
             focal_mm, focal_35mm_mm, folder
         FROM files WHERE id IN (SELECT value FROM json_each(?1))",
    )?;
    let raw: Vec<i64> = ids.iter().map(|file| file.0).collect();
    let mut rows = statement.query([json_list(&raw)?])?;
    let places = places();
    let mut found: HashMap<i64, FileFacts> = HashMap::with_capacity(ids.len());
    while let Some(row) = rows.next()? {
        let kind: String = row.get(3)?;
        let facts = FileFacts {
            path: row.get(1)?,
            name: row.get(2)?,
            kind: SourceTag::parse(&kind).ok_or_else(|| {
                Error::incompatible(format!("index file row: unknown kind {kind}"))
            })?,
            dimensions: dimensions(row, 4)?,
            orientation: row.get::<_, Option<u8>>(6)?.and_then(ExifOrientation::new),
            capture: row.get(7)?,
            place: position(row, 8)?.and_then(|position| places.nearest(&position)),
            camera: camera_label(row, 11)?,
            lens: row.get(13)?,
            exposure: exposure(row, 14)?,
            folder: row.get(20)?,
        };
        found.insert(row.get(0)?, facts);
    }
    drop(rows);
    drop(statement);
    let previews = grid(connection, &ids)?;
    let offline: Vec<String> = connection
        .prepare_cached("SELECT path FROM roots WHERE offline = 1")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    drop(index);
    let paths: Vec<&str> = found.values().map(|facts| facts.path.as_str()).collect();
    let paths = json_list(&paths)?;
    let picked: std::collections::HashSet<String> = service
        .connection
        .prepare_cached("SELECT path FROM picks WHERE path IN (SELECT value FROM json_each(?1))")?
        .query_map([&paths], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let developed: HashMap<String, String> = service
        .connection
        .prepare_cached(
            "SELECT canonical_locator, id FROM assets
             WHERE canonical_locator IN (SELECT value FROM json_each(?1)) AND removed_ms IS NULL",
        )?
        .query_map([&paths], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut answer = Vec::with_capacity(ids.len());
    for (offset, (file, preview)) in ids.iter().zip(previews).enumerate() {
        let position = from + offset as u32;
        let facts = found.remove(&file.0).ok_or_else(|| gone(position))?;
        let availability = if offline.iter().any(|root| within(&facts.folder, root)) {
            FileAvailability::Offline
        } else {
            FileAvailability::Available
        };
        answer.push(ViewRow {
            position,
            item: RowItem::File { file_id: *file },
            picked: picked.contains(&facts.path),
            developed_as: developed
                .get(&facts.path)
                .map(|id| AssetId::parse(id.as_str()))
                .transpose()?,
            path: facts.path.into(),
            file_name: facts.name,
            kind: facts.kind,
            dimensions: facts.dimensions,
            orientation: facts.orientation,
            capture: facts.capture,
            place: facts.place,
            camera: facts.camera,
            lens: facts.lens,
            exposure: facts.exposure,
            moment: moment_of(moments, position),
            edited: false,
            availability,
            preview,
        });
    }
    Ok(answer)
}

/// What one photograph's rows give a view row.
struct PhotoFacts {
    asset_id: AssetId,
    entry_id: Option<EntryId>,
    row: ViewRow,
}

fn photo_rows(
    service: &EditorService,
    window: &[ViewItem],
    from: u32,
    moments: &[Moment],
) -> Result<Vec<ViewRow>, Error> {
    let rows_ids: Vec<i64> = window
        .iter()
        .map(|item| match item {
            ViewItem::Photo(row) => row.0,
            ViewItem::File(_) => unreachable!("a view over photographs holds photographs"),
        })
        .collect();
    let mut statement = service.connection.prepare_cached(
        "SELECT a.row_id, a.id, a.locator, a.file_name, a.source_kind, a.width, a.height,
             a.availability, s.current_entry_id, c.local_text, c.place, c.make, c.model, c.lens,
             c.exposure_time_s, c.f_number, c.iso, c.exposure_bias_ev, c.focal_mm,
             c.focal_35mm_mm, c.width, c.height, c.orientation,
             EXISTS(SELECT 1 FROM entries e WHERE e.asset_id = a.id AND e.sequence > 0)
         FROM assets a LEFT JOIN capture c ON c.asset_row = a.row_id
             LEFT JOIN asset_state s ON s.asset_id = a.id
         WHERE a.row_id IN (SELECT value FROM json_each(?1))",
    )?;
    let mut rows = statement.query([json_list(&rows_ids)?])?;
    let mut found: HashMap<i64, PhotoFacts> = HashMap::with_capacity(rows_ids.len());
    while let Some(row) = rows.next()? {
        let asset_id = AssetId::parse(row.get::<_, String>(1)?)?;
        let kind: String = row.get(4)?;
        let availability: String = row.get(7)?;
        let entry_id = row
            .get::<_, Option<String>>(8)?
            .map(EntryId::parse)
            .transpose()?;
        let interpreted = dimensions(row, 5)?;
        let row_facts = ViewRow {
            position: 0,
            item: RowItem::Photo {
                asset_id: asset_id.clone(),
            },
            path: row.get::<_, String>(2)?.into(),
            file_name: row.get(3)?,
            kind: SourceTag::parse(&kind)
                .ok_or_else(|| Error::incompatible(format!("photograph: unknown kind {kind}")))?,
            dimensions: dimensions(row, 20)?.or(interpreted),
            orientation: row.get::<_, Option<u8>>(22)?.and_then(ExifOrientation::new),
            capture: row.get(9)?,
            place: row.get(10)?,
            camera: camera_label(row, 11)?,
            lens: row.get(13)?,
            exposure: exposure(row, 14)?,
            moment: None,
            picked: false,
            developed_as: None,
            edited: row.get(23)?,
            availability: FileAvailability::parse(&availability).ok_or_else(|| {
                Error::incompatible(format!("photograph: unknown availability {availability}"))
            })?,
            preview: PreviewState::Pending,
        };
        found.insert(
            row.get(0)?,
            PhotoFacts {
                asset_id,
                entry_id,
                row: row_facts,
            },
        );
    }
    drop(rows);
    drop(statement);
    let mut answer = Vec::with_capacity(rows_ids.len());
    let mut wanted = Vec::with_capacity(rows_ids.len());
    for (offset, row_id) in rows_ids.iter().enumerate() {
        let position = from + offset as u32;
        let facts = found.remove(row_id).ok_or_else(|| gone(position))?;
        wanted.push((facts.asset_id, facts.entry_id));
        answer.push(ViewRow {
            position,
            moment: moment_of(moments, position),
            ..facts.row
        });
    }
    let index = service.index()?;
    for (row, preview) in answer
        .iter_mut()
        .zip(photo_grid_states(index.connection(), &wanted))
    {
        row.preview = preview;
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_types::MomentKind;

    #[test]
    fn browse_a_position_finds_its_moment_and_frame() {
        let moment = |start, len| Moment {
            kind: MomentKind::Burst,
            evidence: None,
            steps_ev: vec![],
            span_ms: 0,
            start,
            len,
        };
        let moments = [moment(2, 3), moment(5, 2), moment(10, 4)];
        let at = |position| moment_of(&moments, position).map(|found| (found.index, found.frame));
        assert_eq!(at(0), None);
        assert_eq!(at(2), Some((0, 0)));
        assert_eq!(at(4), Some((0, 2)));
        assert_eq!(at(5), Some((1, 0)));
        assert_eq!(at(7), None);
        assert_eq!(at(13), Some((2, 3)));
        assert_eq!(at(14), None);
        assert_eq!(moment_of(&[], 0), None);
    }
}
