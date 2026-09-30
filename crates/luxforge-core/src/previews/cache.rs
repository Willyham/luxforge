//! The previews cache: where a file's tiers live under `<catalog>.index/previews/`, the rows of the
//! index's `previews` table that record them, when a row may be served, how a tier is written and
//! replaced, and the byte budget the loupe and large tiers share.
//!
//! **Validity.** A file's preview is valid only while the signature it records (length,
//! modification time, file identity) equals the index row's current signature, compared column for
//! column. A stale row is never served: the owner's reads leave it out, and the worker that next
//! makes that tier removes it, the row first and then its file. So is a row whose file is missing
//! or empty.
//!
//! **Names.** Each file is named from what made it — the file's row, the tier, the origin and a
//! hash of the signature (`files/<xx>/<file>-<tier>-<origin>-<hash>.jpg`) — so a replacement is a
//! new path and never overwrites one a client may be reading, and [`PreviewInfo::key`] names
//! exactly what the preview was made from.
//!
//! **Writes.** A tier is written to a temporary file beside its final name and renamed into place,
//! so no reader sees a partial file; its row is written only after the rename, and the file it
//! replaces is removed only after its row changed. Nothing is flushed: previews are a disposable
//! cache, and `atomic_file::flush` is for what must survive a crash, which a preview need not — a
//! lost one costs only the time to read it again.
//!
//! **Budget.** Grid tiers are kept. Loupe tiers and developed photographs' large tiers (in
//! `photo_previews`) share one byte budget, least recently used out first ([`Store::evict`]).
//!
//! **Fingerprints.** A complete grid tier's brightness fingerprint (`bracket.rs`) is made from the
//! tier's own bytes as it is written, and kept in `grid_fingerprints` in the same transaction as
//! the tier's row, naming the tier's file: it is valid while the file's grid row names that file
//! and is valid itself, and a thumbnail stage's row removes it. Its own table keeps the `previews`
//! rows small for the scans the budget and `catalog.info` make.
use super::{bracket::Fingerprint, extract::Made};
use crate::{
    Error,
    atomic_file::file_error,
    catalog_types::{
        FileId, FileIdentity, FileSignature, PreviewInfo, PreviewItem, PreviewOrigin, PreviewState,
        PreviewTier,
    },
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

/// The files' tiers' directory inside the preview cache.
const FILES_DIR: &str = "files";

/// A served loupe tier's `last_used_ms` is written at most this often, so reading the loupe while
/// stepping through a burst costs no write per step.
pub(crate) const TOUCH_INTERVAL_MS: i64 = 60_000;

/// The columns every row read selects, in [`Stored`]'s order.
const COLUMNS: &str = "file_id, tier, byte_len, modified_ns, device, inode, path, width, height, \
                       bytes, origin, last_used_ms";

/// One cached tier of a file, as its row records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CachedPreview {
    pub file: FileId,
    pub tier: PreviewTier,
    /// The file's signature the preview was made from.
    pub signature: FileSignature,
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub bytes: u64,
    pub origin: PreviewOrigin,
    pub last_used_ms: i64,
}

impl CachedPreview {
    /// Whether the tier is complete: a loupe tier always, a grid tier once it holds more than the
    /// thumbnail stage the grid draws first.
    pub(crate) fn complete(&self) -> bool {
        self.origin != PreviewOrigin::ExifThumbnail
    }

    /// The preview as `preview.read` answers it.
    pub(crate) fn info(&self) -> PreviewInfo {
        PreviewInfo {
            item: PreviewItem::File { file_id: self.file },
            tier: self.tier,
            path: self.path.clone(),
            width: self.width,
            height: self.height,
            origin: self.origin,
            bytes: self.bytes,
            key: key(self.file, &self.signature, self.tier, self.origin),
        }
    }
}

/// What a file's preview was made from: its row and signature, then which of its images.
pub(crate) fn key(
    file: FileId,
    signature: &FileSignature,
    tier: PreviewTier,
    origin: PreviewOrigin,
) -> String {
    format!(
        "file:{}:{}:{}:{}",
        file.0,
        signature_hash(signature),
        tier.as_str(),
        origin.as_str()
    )
}

/// The first 64 bits of the SHA-256 of a signature's columns, as 16 lowercase hex digits.
fn signature_hash(signature: &FileSignature) -> String {
    let mut hash = Sha256::new();
    hash.update(signature.len.to_le_bytes());
    hash.update(signature.modified_ns.to_le_bytes());
    match signature.identity {
        Some(identity) => {
            hash.update([1]);
            hash.update(identity.device.to_le_bytes());
            hash.update(identity.inode.to_le_bytes());
        }
        None => hash.update([0]),
    }
    hash.finalize()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Where a file's tier made from `signature` with `origin` lives in the cache at `previews`: one of
/// 256 folders by the file's row, so no folder holds more than a few hundred files at the design's
/// scale.
pub(crate) fn preview_path(
    previews: &Path,
    file: FileId,
    signature: &FileSignature,
    tier: PreviewTier,
    origin: PreviewOrigin,
) -> PathBuf {
    previews
        .join(FILES_DIR)
        .join(format!("{:02x}", file.0 & 0xff))
        .join(format!(
            "{}-{}-{}-{}.jpg",
            file.0,
            tier.as_str(),
            origin.as_str(),
            signature_hash(signature)
        ))
}

/// A `previews` row as SQLite hands it back, before its values are checked into their types.
struct Stored {
    file: i64,
    tier: String,
    byte_len: i64,
    modified_ns: i64,
    device: Option<i64>,
    inode: Option<i64>,
    path: String,
    width: u32,
    height: u32,
    bytes: i64,
    origin: String,
    last_used_ms: i64,
}

impl Stored {
    /// The row starting at column `at`.
    fn read(row: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<Self> {
        Ok(Self {
            file: row.get(at)?,
            tier: row.get(at + 1)?,
            byte_len: row.get(at + 2)?,
            modified_ns: row.get(at + 3)?,
            device: row.get(at + 4)?,
            inode: row.get(at + 5)?,
            path: row.get(at + 6)?,
            width: row.get(at + 7)?,
            height: row.get(at + 8)?,
            bytes: row.get(at + 9)?,
            origin: row.get(at + 10)?,
            last_used_ms: row.get(at + 11)?,
        })
    }

    fn into_preview(self) -> Result<CachedPreview, Error> {
        let bad = |what: &str| Error::incompatible(format!("index preview row: {what}"));
        let tier = PreviewTier::ALL
            .into_iter()
            .find(|tier| tier.as_str() == self.tier)
            .ok_or_else(|| bad("unknown tier"))?;
        Ok(CachedPreview {
            file: FileId(self.file),
            tier,
            signature: FileSignature {
                len: u64::try_from(self.byte_len).map_err(|_| bad("negative length"))?,
                modified_ns: self.modified_ns,
                identity: FileIdentity::from_columns(self.device, self.inode),
            },
            path: self.path.into(),
            width: self.width,
            height: self.height,
            bytes: u64::try_from(self.bytes).map_err(|_| bad("negative size"))?,
            origin: PreviewOrigin::parse(&self.origin).ok_or_else(|| bad("unknown origin"))?,
            last_used_ms: self.last_used_ms,
        })
    }
}

/// A signature from the four columns a table stores it in.
pub(super) fn signature(
    len: i64,
    modified_ns: i64,
    device: Option<i64>,
    inode: Option<i64>,
) -> FileSignature {
    FileSignature {
        len: len.max(0) as u64,
        modified_ns,
        identity: FileIdentity::from_columns(device, inode),
    }
}

/// What the owner reads to answer `preview.read` for one file: the index row's signature now, and
/// its tiers still valid for it. Stale rows are left out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileTiers {
    pub signature: FileSignature,
    pub grid: Option<CachedPreview>,
    pub loupe: Option<CachedPreview>,
}

impl FileTiers {
    pub(crate) fn tier(&self, tier: PreviewTier) -> Option<&CachedPreview> {
        match tier {
            PreviewTier::Grid => self.grid.as_ref(),
            PreviewTier::Loupe => self.loupe.as_ref(),
            PreviewTier::Large => None,
        }
    }
}

/// The file's signature and its valid tiers, in one query; none when the index has no such file.
pub(crate) fn file_tiers(
    connection: &Connection,
    file: FileId,
) -> Result<Option<FileTiers>, Error> {
    // The file's signature, then the columns of [`Stored`] from each of its preview rows.
    let mut statement = connection.prepare_cached(
        "SELECT f.byte_len, f.modified_ns, f.device, f.inode, p.file_id, p.tier, p.byte_len,
             p.modified_ns, p.device, p.inode, p.path, p.width, p.height, p.bytes, p.origin,
             p.last_used_ms
         FROM files f LEFT JOIN previews p ON p.file_id = f.id
         WHERE f.id = ?1",
    )?;
    let mut rows = statement.query([file.0])?;
    let mut found: Option<FileTiers> = None;
    while let Some(row) = rows.next()? {
        let current = signature(row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        let tiers = found.get_or_insert(FileTiers {
            signature: current,
            grid: None,
            loupe: None,
        });
        if row.get::<_, Option<i64>>(4)?.is_none() {
            continue;
        }
        let preview = Stored::read(row, 4)?.into_preview()?;
        if preview.signature != current {
            continue;
        }
        match preview.tier {
            PreviewTier::Grid => tiers.grid = Some(preview),
            PreviewTier::Loupe => tiers.loupe = Some(preview),
            PreviewTier::Large => {}
        }
    }
    Ok(found)
}

/// Record that a file's loupe tier was served now, unless that was recorded within
/// [`TOUCH_INTERVAL_MS`]. The least recently used loupe and large tiers leave the budget first.
pub(crate) fn touch(connection: &Connection, file: FileId, now_ms: i64) -> Result<(), Error> {
    connection
        .prepare_cached(
            "UPDATE previews SET last_used_ms = ?2
             WHERE file_id = ?1 AND tier = 'loupe' AND last_used_ms <= ?2 - ?3",
        )?
        .execute(params![file.0, now_ms, TOUCH_INTERVAL_MS])?;
    Ok(())
}

/// One file's grid tier as the grid can draw it now, and the index row's signature it was judged
/// against: `None` for a file the index does not hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GridRow {
    pub state: PreviewState,
    pub signature: Option<FileSignature>,
}

/// Each file's grid state, in the order given, in one query: [`PreviewState::Ready`] for a valid
/// complete grid tier, [`PreviewState::Thumbnail`] for a valid thumbnail stage, and
/// [`PreviewState::Pending`] otherwise, including a stale row and a file the index does not hold.
/// What only the lane remembers — that a file has no usable preview — is its to add
/// (`PreviewsLane::grid_states` on the owner).
pub(crate) fn grid_rows(connection: &Connection, files: &[FileId]) -> Result<Vec<GridRow>, Error> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let ids = serde_json::to_string(&files.iter().map(|file| file.0).collect::<Vec<_>>())
        .map_err(|error| Error::internal(format!("file ids: {error}")))?;
    let mut statement = connection.prepare_cached(
        "SELECT f.byte_len, f.modified_ns, f.device, f.inode,
                p.byte_len, p.modified_ns, p.device, p.inode, p.origin
         FROM json_each(?1) j
         LEFT JOIN files f ON f.id = j.value
         LEFT JOIN previews p ON p.file_id = f.id AND p.tier = 'grid'
         ORDER BY j.key",
    )?;
    let rows = statement.query_map([ids], |row| {
        let current = match row.get::<_, Option<i64>>(0)? {
            Some(len) => Some(signature(len, row.get(1)?, row.get(2)?, row.get(3)?)),
            None => None,
        };
        let recorded = match row.get::<_, Option<i64>>(4)? {
            Some(len) => Some(signature(len, row.get(5)?, row.get(6)?, row.get(7)?)),
            None => None,
        };
        let origin: Option<String> = row.get(8)?;
        Ok((current, recorded, origin))
    })?;
    let mut states = Vec::with_capacity(files.len());
    for row in rows {
        let (current, recorded, origin) = row?;
        let state = match (
            current,
            recorded,
            origin.as_deref().and_then(PreviewOrigin::parse),
        ) {
            (Some(current), Some(recorded), Some(origin)) if current == recorded => {
                if origin == PreviewOrigin::ExifThumbnail {
                    PreviewState::Thumbnail
                } else {
                    PreviewState::Ready
                }
            }
            _ => PreviewState::Pending,
        };
        states.push(GridRow {
            state,
            signature: current,
        });
    }
    Ok(states)
}

/// Each file's grid state, in the order given, in one query (see [`grid_rows`]). For lane D's
/// `browse.rows`; the owner's `PreviewsLane::grid_states` also reports `unavailable` for files the
/// lane found no usable preview in.
#[allow(dead_code, reason = "lane D's browse.rows calls it as it lands")]
pub(crate) fn grid_states(
    connection: &Connection,
    files: &[FileId],
) -> Result<Vec<PreviewState>, Error> {
    Ok(grid_rows(connection, files)?
        .into_iter()
        .map(|row| row.state)
        .collect())
}

/// The preview cache's size by tier, for `catalog.info`: every recorded file, stale or not, since
/// each is on disk until the lane removes it.
#[allow(dead_code, reason = "lane C's catalog.info reads it as it lands")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CacheBytes {
    /// Files' and developed photographs' grid tiers.
    pub grid: u64,
    /// Files' loupe tiers.
    pub loupe: u64,
    /// Developed photographs' large tiers.
    pub large: u64,
    /// How many files the rows record.
    pub files: u64,
}

#[allow(dead_code, reason = "lane C's catalog.info reads it as it lands")]
impl CacheBytes {
    pub(crate) fn total(&self) -> u64 {
        self.grid + self.loupe + self.large
    }
}

/// The preview cache's size by tier, in one query.
#[allow(dead_code, reason = "lane C's catalog.info calls it as it lands")]
pub(crate) fn cache_bytes(connection: &Connection) -> Result<CacheBytes, Error> {
    let (grid, loupe, large, files): (i64, i64, i64, i64) = connection.query_row(
        "SELECT
             (SELECT COALESCE(SUM(bytes), 0) FROM previews WHERE tier = 'grid')
               + (SELECT COALESCE(SUM(bytes), 0) FROM photo_previews WHERE tier = 'grid'),
             (SELECT COALESCE(SUM(bytes), 0) FROM previews WHERE tier = 'loupe'),
             (SELECT COALESCE(SUM(bytes), 0) FROM photo_previews WHERE tier = 'large'),
             (SELECT COUNT(*) FROM previews) + (SELECT COUNT(*) FROM photo_previews)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let count = |value: i64| value.max(0) as u64;
    Ok(CacheBytes {
        grid: count(grid),
        loupe: count(loupe),
        large: count(large),
        files: count(files),
    })
}

/// Remove a cache file. One already gone is what was wanted, and one that cannot be removed has
/// nobody to be reported to: no row names it any more, so it costs only its bytes.
pub(super) fn remove(path: &Path) {
    let _ = fs::remove_file(path);
}

/// Whether a cache file holds something to serve: present and not empty.
pub(crate) fn intact(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
}

/// A worker's side of the cache: its own connection to the index and the cache's directory. The
/// owner never writes a preview; it only reads rows and records use.
pub(crate) struct Store {
    connection: Connection,
    /// `<catalog>.index/previews`.
    dir: PathBuf,
}

impl Store {
    pub(crate) fn new(connection: Connection, dir: PathBuf) -> Self {
        Self { connection, dir }
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }

    /// The connection, for a write of a developed photograph's rows (`photos.rs`).
    pub(crate) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// `<catalog>.index/previews`.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file's `tier` still valid for `signature`, having first removed the row and its file
    /// when the row was made from another signature, or its file is missing or empty.
    pub(crate) fn current(
        &mut self,
        file: FileId,
        tier: PreviewTier,
        signature: &FileSignature,
    ) -> Result<Option<CachedPreview>, Error> {
        let stored = self
            .connection
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM previews WHERE file_id = ?1 AND tier = ?2"
            ))?
            .query_row(params![file.0, tier.as_str()], |row| Stored::read(row, 0))
            .optional()?;
        let Some(stored) = stored else {
            return Ok(None);
        };
        let preview = stored.into_preview()?;
        if preview.signature == *signature && intact(&preview.path) {
            return Ok(Some(preview));
        }
        // The row goes first, so no reader is handed a path whose file is about to go.
        self.connection
            .prepare_cached("DELETE FROM previews WHERE file_id = ?1 AND tier = ?2 AND path = ?3")?
            .execute(params![
                file.0,
                tier.as_str(),
                preview.path.to_string_lossy()
            ])?;
        remove(&preview.path);
        Ok(None)
    }

    /// Write `made` as the file's `tier`, made from `signature`, last used `now_ms`: the file
    /// through a temporary file and a rename in its own directory, then its row, then the removal
    /// of the file the row named before, if it named another.
    pub(crate) fn write(
        &mut self,
        file: FileId,
        tier: PreviewTier,
        signature: &FileSignature,
        made: &Made,
        now_ms: i64,
    ) -> Result<CachedPreview, Error> {
        let path = preview_path(&self.dir, file, signature, tier, made.origin);
        let folder = path.parent().expect("a preview's path has a folder");
        fs::create_dir_all(folder).map_err(|error| file_error(folder.display(), error.kind()))?;
        let mut temporary = path.clone().into_os_string();
        temporary.push(".tmp");
        let temporary = PathBuf::from(temporary);
        // Never flushed: the cache is disposable (the module's documentation says why). The
        // rename is what a reader relies on: it sees the whole file or none.
        if let Err(error) = fs::write(&temporary, &made.jpeg) {
            remove(&temporary);
            return Err(file_error(temporary.display(), error.kind()));
        }
        if let Err(error) = fs::rename(&temporary, &path) {
            remove(&temporary);
            return Err(file_error(path.display(), error.kind()));
        }
        let preview = CachedPreview {
            file,
            tier,
            signature: *signature,
            path,
            width: made.width,
            height: made.height,
            bytes: made.jpeg.len() as u64,
            origin: made.origin,
            last_used_ms: now_ms,
        };
        // A complete grid tier's fingerprint, from the tier's own bytes (a reduced decode of at
        // most the tier's pixels), so whichever path made the tier gives it one. A tier it cannot
        // decode has none, which only leaves its runs to the metadata.
        let fingerprint = (tier == PreviewTier::Grid
            && made.origin != PreviewOrigin::ExifThumbnail)
            .then(|| Fingerprint::of_jpeg(&made.jpeg))
            .flatten();
        let replaced = match self.record(&preview, fingerprint.as_ref()) {
            Ok(replaced) => replaced,
            Err(error) => {
                remove(&preview.path);
                return Err(error);
            }
        };
        if let Some(replaced) = replaced
            && replaced != preview.path
        {
            remove(&replaced);
        }
        Ok(preview)
    }

    /// Write the row of `preview` and, for a grid tier, its `fingerprint` row (removed when it has
    /// none, as a thumbnail stage has not), answering the path the row named before, if there was
    /// one. The transaction takes the write lock before it reads: under write-ahead logging a read
    /// transaction another connection has written past cannot become a write, and would fail at
    /// once rather than wait its turn.
    fn record(
        &mut self,
        preview: &CachedPreview,
        fingerprint: Option<&Fingerprint>,
    ) -> Result<Option<PathBuf>, Error> {
        let identity = preview.signature.identity.map(FileIdentity::to_columns);
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let replaced: Option<String> = tx
            .prepare_cached("SELECT path FROM previews WHERE file_id = ?1 AND tier = ?2")?
            .query_row(params![preview.file.0, preview.tier.as_str()], |row| {
                row.get(0)
            })
            .optional()?;
        tx.prepare_cached(
            "INSERT INTO previews (file_id, tier, byte_len, modified_ns, device, inode, path,
                 width, height, bytes, origin, last_used_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(file_id, tier) DO UPDATE SET byte_len = excluded.byte_len,
                 modified_ns = excluded.modified_ns, device = excluded.device,
                 inode = excluded.inode, path = excluded.path, width = excluded.width,
                 height = excluded.height, bytes = excluded.bytes, origin = excluded.origin,
                 last_used_ms = excluded.last_used_ms",
        )?
        .execute(params![
            preview.file.0,
            preview.tier.as_str(),
            i64::try_from(preview.signature.len)
                .map_err(|_| Error::resource_limit("file length exceeds index range"))?,
            preview.signature.modified_ns,
            identity.map(|(device, _)| device),
            identity.map(|(_, inode)| inode),
            preview.path.to_string_lossy(),
            preview.width,
            preview.height,
            preview.bytes as i64,
            preview.origin.as_str(),
            preview.last_used_ms,
        ])?;
        if preview.tier == PreviewTier::Grid {
            match fingerprint {
                Some(fingerprint) => tx
                    .prepare_cached(
                        "INSERT OR REPLACE INTO grid_fingerprints (file_id, path, fingerprint)
                         VALUES (?1, ?2, ?3)",
                    )?
                    .execute(params![
                        preview.file.0,
                        preview.path.to_string_lossy(),
                        fingerprint.as_bytes()
                    ])?,
                None => tx
                    .prepare_cached("DELETE FROM grid_fingerprints WHERE file_id = ?1")?
                    .execute([preview.file.0])?,
            };
        }
        tx.commit()?;
        Ok(replaced.map(PathBuf::from))
    }

    /// Keep the loupe tiers and developed photographs' large tiers within `budget` bytes together,
    /// removing the least recently used first, never the one at `keep` (the tier just written).
    /// Grid tiers are never removed. The rows go in one transaction that takes the write lock
    /// first, so two workers never count the same bytes out twice; their files go after it
    /// commits. Answers how many were removed.
    pub(crate) fn evict(&mut self, budget: u64, keep: &Path) -> Result<usize, Error> {
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let total: i64 = tx.query_row(
            "SELECT (SELECT COALESCE(SUM(bytes), 0) FROM previews WHERE tier = 'loupe')
                  + (SELECT COALESCE(SUM(bytes), 0) FROM photo_previews WHERE tier = 'large')",
            [],
            |row| row.get(0),
        )?;
        let mut over = total.max(0) as u64;
        if over <= budget {
            return Ok(0);
        }
        over -= budget;
        let keep = keep.to_string_lossy();
        let mut victims: Vec<(String, u64)> = Vec::new();
        {
            let mut statement = tx.prepare_cached(
                "SELECT path, bytes, last_used_ms FROM previews WHERE tier = 'loupe'
                 UNION ALL
                 SELECT path, bytes, last_used_ms FROM photo_previews WHERE tier = 'large'
                 ORDER BY last_used_ms, path",
            )?;
            let mut rows = statement.query([])?;
            while over > 0
                && let Some(row) = rows.next()?
            {
                let path: String = row.get(0)?;
                if path == keep {
                    continue;
                }
                let bytes = row.get::<_, i64>(1)?.max(0) as u64;
                over = over.saturating_sub(bytes);
                victims.push((path, bytes));
            }
        }
        for (path, _) in &victims {
            tx.prepare_cached("DELETE FROM previews WHERE path = ?1")?
                .execute([path])?;
            tx.prepare_cached("DELETE FROM photo_previews WHERE path = ?1")?
                .execute([path])?;
        }
        tx.commit()?;
        for (path, _) in &victims {
            remove(Path::new(path));
        }
        Ok(victims.len())
    }
}

/// How the grid tiers of `files` stand, for the view job: the files the index holds whose grid is
/// not complete and valid, with their current signatures. One query.
pub(crate) fn grids_wanted(
    connection: &Connection,
    files: &[FileId],
) -> Result<Vec<(FileId, FileSignature)>, Error> {
    let rows = grid_rows(connection, files)?;
    let mut seen = HashSet::with_capacity(files.len());
    Ok(files
        .iter()
        .zip(rows)
        .filter_map(|(file, row)| match (row.state, row.signature) {
            (PreviewState::Ready, _) | (_, None) => None,
            (_, Some(signature)) => seen.insert(*file).then_some((*file, signature)),
        })
        .collect())
}
