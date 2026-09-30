//! A developed photograph's previews in the cache: the rows of the index's `photo_previews` table
//! and their files under `<catalog>.index/previews/photos/`, as the preview lane writes, serves and
//! collects them (`docs/design/catalog.md`, "Rendered previews").
//!
//! - **Rendered tiers** (origin `rendered`): the grid (512 px) and large (2048 px) tiers of one
//!   entry at the renderer generation that made them ([`RenderedKey`]), written by the render
//!   worker (`renders.rs`).
//! - **Camera previews** (origin `embedded`, renderer [`CAMERA_RENDERER`]): until a photograph's
//!   first render, its tiers are its original's camera preview, made by an extraction worker
//!   (`camera.rs`) as a browsed file's are and keyed by the entry that was current when it was
//!   made. A camera preview is the same whichever entry is current, so any entry's serves as a
//!   fallback. A render of the same entry replaces it (the row's key is the same), and a render of
//!   a later entry collects it with the rest of the photograph's stale rows.
//! - **What is served.** A tier is ready only when it is the entry's rendered tier at this
//!   generation ([`PhotoRow::is_current`]) and its file is intact. Meanwhile the best other row is
//!   the fallback ([`fallbacks`]), labelled by its own key and origin.
//! - **Writes**, as a file's tiers are written: a temporary file renamed into place, then the row,
//!   then the removal of the file the row named before. Never flushed: the cache is disposable. A
//!   camera preview never replaces a rendered row and is not written once any render of its tier
//!   exists, so a camera preview finishing after a render cannot hide it.
//! - **Collection** ([`collect`]): the rows go first, each only while it still names the same file,
//!   and then their files, so no reader is handed a path whose file is about to go.
//! - **Forgetting** ([`forget`]): a photograph that leaves the catalog loses every row, on the
//!   owner, a page of photographs to a short transaction, and then their files, on a short-lived
//!   thread ([`remove_files`]). Its work is cancelled first, and a write checks its cancel while it
//!   holds the write lock, so no row of it is written after.
//!
//! Large tiers, rendered or camera previews, share the loupe tiers' byte budget through
//! [`Store::evict`]; grid tiers are kept.
use super::{
    cache::{self, Store},
    rendered::{
        PhotoPreviewRow, RENDERER_GENERATION, RenderedKey, is_current, other_generation_rows,
    },
};
use crate::{
    AssetId, EntryId, Error,
    atomic_file::file_error,
    catalog_types::{PreviewInfo, PreviewItem, PreviewOrigin, PreviewState, PreviewTier},
    jobs::JobControl,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    thread,
};

/// The `renderer` column of a camera preview's row: made by no renderer.
pub(crate) const CAMERA_RENDERER: i64 = 0;

/// One row of a photograph's in `photo_previews`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PhotoRow {
    pub entry_id: EntryId,
    pub tier: PreviewTier,
    pub renderer: i64,
    pub origin: PreviewOrigin,
    /// Absolute, under `<catalog>.index/previews/photos/`.
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    /// A rendered tier made through an approximate proxy
    /// ([`RenderedTier::approximate`](super::rendered::RenderedTier::approximate)); never a camera
    /// preview.
    pub approximate: bool,
}

impl PhotoRow {
    pub(crate) fn rendered(&self) -> bool {
        self.origin == PreviewOrigin::Rendered
    }

    /// Whether this row of `asset_id`'s is `tier` of `entry` rendered at this build's generation,
    /// by its key ([`is_current`]): the only row a `preview.read` of that tier answers ready.
    pub(crate) fn is_current(
        &self,
        asset_id: &AssetId,
        entry: &EntryId,
        tier: PreviewTier,
    ) -> bool {
        self.rendered() && is_current(&self.info(asset_id).key, asset_id, entry, tier)
    }

    /// The preview this row is, of `asset_id`, keyed by what made it.
    pub(crate) fn info(&self, asset_id: &AssetId) -> PreviewInfo {
        let key = if self.rendered() {
            RenderedKey {
                asset_id: asset_id.clone(),
                entry_id: self.entry_id.clone(),
                tier: self.tier,
                generation: u32::try_from(self.renderer).unwrap_or(u32::MAX),
            }
            .preview_key()
        } else {
            camera_key(asset_id, &self.entry_id, self.tier)
        };
        PreviewInfo {
            item: PreviewItem::Photo {
                asset_id: asset_id.clone(),
                entry_id: Some(self.entry_id.clone()),
            },
            tier: self.tier,
            path: self.path.clone(),
            width: self.width,
            height: self.height,
            origin: self.origin,
            approximate: self.approximate,
            bytes: self.bytes,
            key,
        }
    }
}

/// The key of a camera preview made while `entry_id` was current:
/// `photo:<asset>:<entry>:<tier>:embedded`.
pub(crate) fn camera_key(asset_id: &AssetId, entry_id: &EntryId, tier: PreviewTier) -> String {
    format!("photo:{asset_id}:{entry_id}:{}:embedded", tier.as_str())
}

/// Where a camera preview made while `entry_id` was current goes, relative to
/// `<catalog>.index/previews/`: beside the entry's rendered tiers, named
/// `<asset>-<entry>-<tier>-embedded.jpg`.
pub(crate) fn camera_file_name(
    asset_id: &AssetId,
    entry_id: &EntryId,
    tier: PreviewTier,
) -> Result<PathBuf, Error> {
    Ok(RenderedKey::new(asset_id, entry_id, tier)?
        .file_name()
        .with_file_name(format!(
            "{asset_id}-{entry_id}-{}-embedded.jpg",
            tier.as_str()
        )))
}

/// A row's columns as SQLite holds them, in [`rows`]' order.
type Columns = (String, String, i64, String, String, u32, u32, i64, bool);

fn parse(columns: Columns) -> Result<PhotoRow, Error> {
    let (entry_id, tier, renderer, origin, path, width, height, bytes, approximate) = columns;
    let unreadable = || Error::catalog("the index holds an unreadable photo preview row");
    Ok(PhotoRow {
        entry_id: EntryId::parse(entry_id).map_err(|_| unreadable())?,
        tier: PreviewTier::ALL
            .into_iter()
            .find(|known| known.as_str() == tier)
            .ok_or_else(unreadable)?,
        renderer,
        origin: PreviewOrigin::parse(&origin).ok_or_else(unreadable)?,
        path: PathBuf::from(path),
        width,
        height,
        bytes: u64::try_from(bytes).map_err(|_| unreadable())?,
        approximate,
    })
}

/// Every row of `asset_id`, both tiers and every entry, in one query over the table's key.
pub(crate) fn rows(index: &Connection, asset_id: &AssetId) -> Result<Vec<PhotoRow>, Error> {
    let mut statement = index.prepare_cached(
        "SELECT entry_id, tier, renderer, origin, path, width, height, bytes, approximate
         FROM photo_previews WHERE asset_id = ?1",
    )?;
    let rows = statement.query_map([asset_id.as_str()], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
            row.get(7)?,
            row.get(8)?,
        ))
    })?;
    rows.map(|row| parse(row?)).collect()
}

/// The rows that may stand in for `entry`'s `tier` while it is rendered, best first: the same tier
/// before the other, a render before a camera preview, the entry's own before another's, this
/// generation's before an older one. The caller serves the first whose file is intact.
pub(crate) fn fallbacks<'r>(
    rows: &'r [PhotoRow],
    asset_id: &AssetId,
    entry: &EntryId,
    tier: PreviewTier,
) -> Vec<&'r PhotoRow> {
    let mut ranked: Vec<&PhotoRow> = rows
        .iter()
        .filter(|row| !row.is_current(asset_id, entry, tier))
        .collect();
    ranked.sort_by_key(|row| {
        (
            row.tier != tier,
            !row.rendered(),
            row.entry_id != *entry,
            row.renderer != i64::from(RENDERER_GENERATION),
        )
    });
    ranked
}

/// Record that `asset_id`'s large tier of `entry_id` was served now, unless that was recorded
/// within [`cache::TOUCH_INTERVAL_MS`], as a served loupe tier is.
pub(crate) fn touch(
    index: &Connection,
    asset_id: &AssetId,
    entry_id: &EntryId,
    now_ms: i64,
) -> Result<(), Error> {
    index
        .prepare_cached(
            "UPDATE photo_previews SET last_used_ms = ?3
             WHERE asset_id = ?1 AND entry_id = ?2 AND tier = 'large'
               AND last_used_ms <= ?3 - ?4",
        )?
        .execute(params![
            asset_id.as_str(),
            entry_id.as_str(),
            now_ms,
            cache::TOUCH_INTERVAL_MS
        ])?;
    Ok(())
}

/// One tier to write: what it is of and the JPEG that holds it.
pub(crate) struct NewTier<'a> {
    pub asset_id: &'a AssetId,
    pub entry_id: &'a EntryId,
    pub tier: PreviewTier,
    /// [`RENDERER_GENERATION`] for a render, [`CAMERA_RENDERER`] for a camera preview.
    pub renderer: i64,
    pub origin: PreviewOrigin,
    /// A rendered tier made through an approximate proxy; `false` for a camera preview.
    pub approximate: bool,
    /// Relative to `<catalog>.index/previews/`.
    pub name: &'a Path,
    pub jpeg: &'a [u8],
    pub width: u32,
    pub height: u32,
    pub now_ms: i64,
    /// The work's control: a cancel refuses the row even once the file is written ([`record`]).
    pub control: &'a JobControl,
}

/// Write `tier` into the cache: its file through a temporary file and a rename, then its row, then
/// the removal of the file the row named before; answers the row. A camera preview is not written,
/// and `None` answered, when a render of its tier already exists for the photograph. Work
/// cancelled by then writes no row and removes its file.
pub(crate) fn write(store: &mut Store, tier: &NewTier<'_>) -> Result<Option<PhotoRow>, Error> {
    let path = store.dir().join(tier.name);
    let folder = path.parent().expect("a preview's path has a folder");
    fs::create_dir_all(folder).map_err(|error| file_error(folder.display(), error.kind()))?;
    let mut temporary = path.clone().into_os_string();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    if let Err(error) = fs::write(&temporary, tier.jpeg) {
        cache::remove(&temporary);
        return Err(file_error(temporary.display(), error.kind()));
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        cache::remove(&temporary);
        return Err(file_error(path.display(), error.kind()));
    }
    let row = PhotoRow {
        entry_id: tier.entry_id.clone(),
        tier: tier.tier,
        renderer: tier.renderer,
        origin: tier.origin,
        path,
        width: tier.width,
        height: tier.height,
        bytes: tier.jpeg.len() as u64,
        approximate: tier.approximate,
    };
    match record(
        store.connection_mut(),
        tier.asset_id,
        &row,
        tier.now_ms,
        tier.control,
    ) {
        Ok(Some(replaced)) => {
            if replaced != row.path {
                cache::remove(&replaced);
            }
        }
        Ok(None) => {}
        Err(Refused::Rendered) => {
            cache::remove(&row.path);
            return Ok(None);
        }
        Err(Refused::Error(error)) => {
            cache::remove(&row.path);
            return Err(error);
        }
    }
    Ok(Some(row))
}

/// Why a row was not recorded.
enum Refused {
    /// A camera preview, and the photograph already has a render of its tier.
    Rendered,
    Error(Error),
}

impl<E: Into<Error>> From<E> for Refused {
    fn from(error: E) -> Self {
        Self::Error(error.into())
    }
}

/// Write `row`, answering the path the row named before, if there was one. The transaction takes
/// the write lock before it reads, as a file's row's does, and checks `control` while it holds it:
/// a photograph that leaves the catalog has its work cancelled before its rows are deleted
/// ([`forget`]), so a row written here is either committed before that deletion, which removes
/// it, or never written.
fn record(
    connection: &mut Connection,
    asset_id: &AssetId,
    row: &PhotoRow,
    now_ms: i64,
    control: &JobControl,
) -> Result<Option<PathBuf>, Refused> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    control.checkpoint()?;
    if !row.rendered() {
        let rendered: Option<i64> = tx
            .prepare_cached(
                "SELECT 1 FROM photo_previews
                 WHERE asset_id = ?1 AND tier = ?2 AND origin = 'rendered' LIMIT 1",
            )?
            .query_row(params![asset_id.as_str(), row.tier.as_str()], |row| {
                row.get(0)
            })
            .optional()?;
        if rendered.is_some() {
            return Err(Refused::Rendered);
        }
    }
    let replaced: Option<String> = tx
        .prepare_cached(
            "SELECT path FROM photo_previews WHERE asset_id = ?1 AND entry_id = ?2 AND tier = ?3",
        )?
        .query_row(
            params![asset_id.as_str(), row.entry_id.as_str(), row.tier.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    tx.prepare_cached(
        "INSERT INTO photo_previews (asset_id, entry_id, tier, renderer, path, width, height,
             bytes, origin, last_used_ms, approximate)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT(asset_id, entry_id, tier) DO UPDATE SET renderer = excluded.renderer,
             path = excluded.path, width = excluded.width, height = excluded.height,
             bytes = excluded.bytes, origin = excluded.origin,
             last_used_ms = excluded.last_used_ms, approximate = excluded.approximate",
    )?
    .execute(params![
        asset_id.as_str(),
        row.entry_id.as_str(),
        row.tier.as_str(),
        row.renderer,
        row.path.to_string_lossy(),
        row.width,
        row.height,
        row.bytes as i64,
        row.origin.as_str(),
        now_ms,
        row.approximate,
    ])?;
    tx.commit()?;
    Ok(replaced.map(PathBuf::from))
}

/// Remove `rows` — as [`stale_rows`](super::rendered::stale_rows) or
/// [`other_generation_rows`] list them — each only while it still names the same file, in one
/// transaction, and then the files of those it removed. Answers how many it removed.
pub(crate) fn collect(store: &mut Store, rows: &[PhotoPreviewRow]) -> Result<usize, Error> {
    if rows.is_empty() {
        return Ok(0);
    }
    let tx = store
        .connection_mut()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut removed = Vec::with_capacity(rows.len());
    for row in rows {
        let deleted = tx
            .prepare_cached(
                "DELETE FROM photo_previews
                 WHERE asset_id = ?1 AND entry_id = ?2 AND tier = ?3 AND path = ?4",
            )?
            .execute(params![
                row.asset_id.as_str(),
                row.entry_id.as_str(),
                row.tier.as_str(),
                row.path.to_string_lossy()
            ])?;
        if deleted > 0 {
            removed.push(&row.path);
        }
    }
    tx.commit()?;
    for path in &removed {
        cache::remove(path);
    }
    Ok(removed.len())
}

/// The most rows of another renderer generation one discard page removes: one short write
/// transaction, so a render's writes never wait long behind it.
pub(crate) const DISCARD_PAGE: usize = 256;

/// Discard every rendered row of another renderer generation with its file, a page of
/// [`DISCARD_PAGE`] at a time, until none is left or `stop` is cancelled. Answers how many.
pub(crate) fn discard_other_generations(
    store: &mut Store,
    stop: &JobControl,
) -> Result<usize, Error> {
    let mut discarded = 0;
    loop {
        stop.checkpoint()?;
        let page = other_generation_rows(store.connection(), DISCARD_PAGE)?;
        if page.is_empty() {
            return Ok(discarded);
        }
        let removed = collect(store, &page)?;
        if removed == 0 {
            // Every row of the page changed under the discard; the next page lists them again.
            return Ok(discarded);
        }
        discarded += removed;
    }
}

/// The most photographs whose rows one transaction of [`forget`] deletes: a short write, so a
/// worker's row never waits long behind a large batch, while a library change's 50,000
/// photographs ([`MAX_LIBRARY_BATCH`](crate::catalog_types::MAX_LIBRARY_BATCH)) take 50.
pub(crate) const FORGET_PAGE: usize = 1_000;

/// Delete every row of `assets` — photographs that left the catalog: every entry, tier, renderer
/// generation and origin — [`FORGET_PAGE`] photographs to a write transaction, each photograph's
/// rows found by the table's key. Answers the files the deleted rows named, for the caller to
/// remove off the owner ([`remove_files`]); no file is touched here. Deleting nothing is not an
/// error, so a second call for the same photographs does nothing.
pub(crate) fn forget(
    connection: &mut Connection,
    assets: &[AssetId],
) -> Result<Vec<PathBuf>, Error> {
    let mut files = Vec::new();
    for page in assets.chunks(FORGET_PAGE) {
        let ids = serde_json::to_string(&page.iter().map(AssetId::as_str).collect::<Vec<_>>())
            .map_err(|error| Error::internal(format!("asset ids: {error}")))?;
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut statement = tx.prepare_cached(
                "DELETE FROM photo_previews WHERE asset_id IN (SELECT value FROM json_each(?1))
                 RETURNING path",
            )?;
            let deleted = statement.query_map([ids], |row| row.get::<_, String>(0))?;
            for path in deleted {
                files.push(PathBuf::from(path?));
            }
        }
        tx.commit()?;
    }
    Ok(files)
}

/// Remove `files`, cache files no row names any more, on a short-lived thread of their own that
/// ends once it has removed them, never on the caller's: the photographs of one library change
/// may name tens of thousands. A thread that cannot be started leaves them, and they cost only
/// their bytes, as a file [`cache::remove`] cannot remove does.
pub(crate) fn remove_files(files: Vec<PathBuf>) {
    if files.is_empty() {
        return;
    }
    let _ = thread::Builder::new()
        .name("luxforge-preview-forget".into())
        .spawn(move || {
            for file in &files {
                cache::remove(file);
            }
        });
}

/// How a photograph's grid stands, from its rows alone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct GridRows {
    /// Its current entry's grid tier, rendered at this generation.
    pub current: bool,
    /// Any rendered grid tier, current or not.
    pub rendered: bool,
    /// Any grid row: a render of any entry or generation, or a camera preview.
    pub any: bool,
}

impl GridRows {
    /// What the grid can draw: ready for the current render, the soft `thumbnail` for a camera
    /// preview only or a render of another entry or generation, `pending` for nothing.
    pub(crate) fn state(self) -> PreviewState {
        if self.current {
            PreviewState::Ready
        } else if self.any {
            PreviewState::Thumbnail
        } else {
            PreviewState::Pending
        }
    }
}

/// How each photograph's grid stands against its current entry, in the order given, in one query.
pub(crate) fn grid_rows(
    index: &Connection,
    photos: &[(AssetId, EntryId)],
) -> Result<Vec<GridRows>, Error> {
    if photos.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::to_string(
        &photos
            .iter()
            .map(|(asset, _)| asset.as_str())
            .collect::<Vec<_>>(),
    )
    .map_err(|error| Error::internal(format!("asset ids: {error}")))?;
    let mut statement = index.prepare_cached(
        "SELECT p.asset_id, p.entry_id, p.renderer, p.origin
         FROM photo_previews p
         WHERE p.tier = 'grid' AND p.asset_id IN (SELECT value FROM json_each(?1))",
    )?;
    let mut found: HashMap<String, Vec<(String, i64, String)>> = HashMap::new();
    let rows = statement.query_map([ids], |row| {
        Ok((row.get(0)?, (row.get(1)?, row.get(2)?, row.get(3)?)))
    })?;
    for row in rows {
        let (asset, grid): (String, (String, i64, String)) = row?;
        found.entry(asset).or_default().push(grid);
    }
    Ok(photos
        .iter()
        .map(|(asset, current)| {
            let grids = found.get(asset.as_str()).map_or(&[][..], Vec::as_slice);
            let rendered = |(_, _, origin): &&(String, i64, String)| origin == "rendered";
            GridRows {
                current: grids.iter().filter(rendered).any(|(entry, renderer, _)| {
                    entry == current.as_str() && *renderer == i64::from(RENDERER_GENERATION)
                }),
                rendered: grids.iter().any(|grid| rendered(&grid)),
                any: !grids.is_empty(),
            }
        })
        .collect())
}
