//! The index database, `<catalog>.index/index.sqlite`: its schema, its format marker, and how it is
//! opened, created and discarded.
//!
//! The index is a cache of what Luxforge read from the files it browses, beside the catalog and
//! never inside it: rebuilding it never touches the catalog's database, and deleting the whole
//! `<catalog>.index/` directory while Luxforge is closed loses nothing but time. So where the
//! catalog refuses an unsupported format by name and keeps its bytes, the index is **discarded and
//! recreated**: on a format mismatch, on a database SQLite cannot read, and when it belongs to
//! another catalog. Discarding removes only what this module names in the index directory (the
//! database with its journal files, and the `previews/` directory whose files the database lists).
//!
//! Several connections may be open at once — the owner's, which views read, and the index lane's
//! workers' ([`IndexDb::connect`]) — so the database uses SQLite's write-ahead log and, as a cache,
//! `synchronous=NORMAL`.
#![allow(
    dead_code,
    reason = "catalog contracts: the row readers are used as the lanes land"
)]
use crate::{
    Error, SourceTag,
    catalog_types::{
        CameraBody, CaptureTime, Dimensions, EmbeddedFormat, EmbeddedImage, ExifOrientation,
        Exposure, FileId, FileIdentity, FileRecord, FileSignature, GeoPosition, HeaderMetadata,
        HeaderState, IndexRoot, VolumeId,
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Format 1: files with their signatures and header columns, the roots listed, and the preview
/// records of files and developed photographs. Any other marker, a database SQLite cannot read, and
/// an index of another catalog are discarded and recreated.
pub const INDEX_FORMAT: i64 = 1;
/// The database's file name inside the index directory.
pub const INDEX_FILE: &str = "index.sqlite";
/// The preview cache's directory inside the index directory; the preview lane (lane B) owns its
/// layout.
pub const PREVIEWS_DIR: &str = "previews";

/// How long a connection waits for another connection's write before it answers `conflict`.
const BUSY_WAIT: Duration = Duration::from_millis(2000);

const SCHEMA: &str = "
    CREATE TABLE index_meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    ) WITHOUT ROWID;
    CREATE TABLE roots (
        id INTEGER PRIMARY KEY,
        path TEXT NOT NULL UNIQUE,
        kind TEXT NOT NULL CHECK (kind IN ('indexed', 'card', 'browsed')),
        volume_id TEXT NOT NULL,
        listed_ms INTEGER,
        file_count INTEGER CHECK (file_count IS NULL OR file_count >= 0),
        offline INTEGER NOT NULL DEFAULT 0 CHECK (offline IN (0, 1))
    );
    CREATE TABLE files (
        id INTEGER PRIMARY KEY,
        path TEXT NOT NULL UNIQUE,
        folder TEXT NOT NULL,
        name TEXT NOT NULL,
        volume_id TEXT NOT NULL,
        byte_len INTEGER NOT NULL CHECK (byte_len >= 0),
        modified_ns INTEGER NOT NULL,
        device INTEGER,
        inode INTEGER,
        kind TEXT NOT NULL CHECK (kind IN ('jpeg', 'raw')),
        header_state TEXT NOT NULL CHECK (header_state IN ('ok', 'pending', 'unreadable')),
        header_error TEXT,
        capture_ms INTEGER,
        local_text TEXT,
        local_day TEXT,
        offset_minutes INTEGER,
        latitude REAL,
        longitude REAL,
        altitude_m REAL,
        make TEXT,
        model TEXT,
        body_serial TEXT,
        lens TEXT,
        exposure_time_s REAL,
        f_number REAL,
        iso INTEGER,
        exposure_bias_ev REAL,
        focal_mm REAL,
        focal_35mm_mm REAL,
        width INTEGER,
        height INTEGER,
        orientation INTEGER,
        thumb_offset INTEGER,
        thumb_len INTEGER,
        thumb_format TEXT CHECK (thumb_format IN ('jpeg', 'rgb8')),
        thumb_width INTEGER,
        thumb_height INTEGER,
        last_seen_ms INTEGER NOT NULL,
        CHECK ((device IS NULL) = (inode IS NULL)),
        CHECK ((header_state = 'unreadable') = (header_error IS NOT NULL)),
        CHECK ((capture_ms IS NULL) = (local_text IS NULL)),
        CHECK ((capture_ms IS NULL) = (local_day IS NULL)),
        CHECK ((latitude IS NULL) = (longitude IS NULL)),
        CHECK ((make IS NULL) = (model IS NULL)),
        CHECK ((width IS NULL) = (height IS NULL)),
        CHECK (orientation IS NULL OR orientation BETWEEN 1 AND 8),
        CHECK ((thumb_offset IS NULL) = (thumb_len IS NULL)),
        CHECK ((thumb_offset IS NULL) = (thumb_format IS NULL)),
        CHECK ((thumb_width IS NULL) = (thumb_height IS NULL)),
        CHECK (thumb_format IS NOT 'rgb8' OR thumb_width IS NOT NULL)
    );
    CREATE INDEX files_by_folder ON files(folder, name);
    CREATE INDEX files_by_time ON files(capture_ms);
    CREATE INDEX files_by_identity ON files(device, inode) WHERE device IS NOT NULL;
    CREATE INDEX files_by_volume ON files(volume_id);
    CREATE INDEX files_by_name_and_length ON files(name, byte_len);
    CREATE TABLE previews (
        file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        tier TEXT NOT NULL CHECK (tier IN ('grid', 'loupe')),
        byte_len INTEGER NOT NULL,
        modified_ns INTEGER NOT NULL,
        device INTEGER,
        inode INTEGER,
        path TEXT NOT NULL UNIQUE,
        width INTEGER NOT NULL,
        height INTEGER NOT NULL,
        bytes INTEGER NOT NULL,
        origin TEXT NOT NULL CHECK (origin IN ('exif-thumbnail', 'embedded', 'developed')),
        last_used_ms INTEGER NOT NULL,
        PRIMARY KEY (file_id, tier)
    ) WITHOUT ROWID;
    CREATE INDEX previews_by_use ON previews(last_used_ms);
    CREATE TABLE photo_previews (
        asset_id TEXT NOT NULL,
        entry_id TEXT NOT NULL,
        tier TEXT NOT NULL CHECK (tier IN ('grid', 'large')),
        renderer INTEGER NOT NULL,
        path TEXT NOT NULL UNIQUE,
        width INTEGER NOT NULL,
        height INTEGER NOT NULL,
        bytes INTEGER NOT NULL,
        origin TEXT NOT NULL CHECK (origin IN ('embedded', 'rendered')),
        last_used_ms INTEGER NOT NULL,
        PRIMARY KEY (asset_id, entry_id, tier)
    ) WITHOUT ROWID;
    CREATE INDEX photo_previews_by_use ON photo_previews(last_used_ms);";

/// The index directory of the catalog at `catalog`: `<stem>.index` beside it, as the artifact
/// directory is `<stem>.artifacts`. One rule, so the owner, the generator and a test name the same
/// directory.
pub fn index_dir(catalog: &Path) -> PathBuf {
    let canonical = catalog
        .canonicalize()
        .unwrap_or_else(|_| catalog.to_path_buf());
    let stem = canonical
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "catalog".into());
    canonical
        .parent()
        .unwrap_or(Path::new(""))
        .join(format!("{stem}.index"))
}

/// What opening found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexOpened {
    /// There was no index: a new, empty one was created.
    Created,
    /// The index was current and is used as it is.
    Opened,
    /// The index could not be used and was replaced by a new, empty one, for this reason.
    Discarded(String),
}

/// One open index database.
#[derive(Debug)]
pub struct IndexDb {
    connection: Connection,
    dir: PathBuf,
}

impl IndexDb {
    /// Open the index in `dir` for the catalog `catalog_id`, creating it when there is none and
    /// discarding it — never the catalog — when it cannot be used.
    pub fn open(dir: &Path, catalog_id: &str) -> Result<(Self, IndexOpened), Error> {
        std::fs::create_dir_all(dir).map_err(|error| {
            Error::file_access(format!(
                "cannot create the index directory: {}",
                error.kind()
            ))
        })?;
        let path = dir.join(INDEX_FILE);
        let existed = path.exists();
        let problem = match Self::try_open(&path, catalog_id) {
            Ok(Some(connection)) => {
                let opened = if existed {
                    IndexOpened::Opened
                } else {
                    IndexOpened::Created
                };
                return Ok((Self::at(connection, dir), opened));
            }
            Ok(None) => return Self::create(dir, catalog_id, IndexOpened::Created),
            Err(problem) => problem,
        };
        discard(dir)?;
        Self::create(dir, catalog_id, IndexOpened::Discarded(problem))
    }

    /// Another connection to this index, configured as the first, for a worker of the index lane.
    pub fn connect(&self) -> Result<Connection, Error> {
        configured(&self.dir.join(INDEX_FILE))
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// The index directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The preview cache's directory, created by the preview lane when it first writes.
    pub fn previews_dir(&self) -> PathBuf {
        self.dir.join(PREVIEWS_DIR)
    }

    fn at(connection: Connection, dir: &Path) -> Self {
        Self {
            connection,
            dir: dir.to_path_buf(),
        }
    }

    /// The current index at `path`, none when there is no database yet, or why it cannot be used.
    fn try_open(path: &Path, catalog_id: &str) -> Result<Option<Connection>, String> {
        if !path.exists() {
            return Ok(None);
        }
        let connection = configured(path).map_err(|error| error.detail)?;
        let format: i64 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if format == 0 {
            let empty: bool = connection
                .query_row(
                    "SELECT NOT EXISTS(SELECT 1 FROM sqlite_schema)",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| error.to_string())?;
            return if empty {
                Ok(None)
            } else {
                Err("the index has no format marker".into())
            };
        }
        if format != INDEX_FORMAT {
            return Err(format!(
                "index format {format} is not supported; expected {INDEX_FORMAT}"
            ));
        }
        let owner: Option<String> = connection
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'catalog_id'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        match owner {
            Some(owner) if owner == catalog_id => Ok(Some(connection)),
            _ => Err("the index belongs to another catalog".into()),
        }
    }

    fn create(
        dir: &Path,
        catalog_id: &str,
        opened: IndexOpened,
    ) -> Result<(Self, IndexOpened), Error> {
        let mut connection = configured(&dir.join(INDEX_FILE))?;
        let tx = connection.transaction()?;
        tx.execute_batch(SCHEMA)?;
        tx.execute(
            "INSERT INTO index_meta (key, value) VALUES ('catalog_id', ?1)",
            [catalog_id],
        )?;
        tx.pragma_update(None, "user_version", INDEX_FORMAT)?;
        tx.commit()?;
        Ok((Self::at(connection, dir), opened))
    }
}

/// A connection to the index database at `path`, with the pragmas every index connection shares.
fn configured(path: &Path) -> Result<Connection, Error> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(BUSY_WAIT)?;
    let mode: String = connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(Error::catalog(format!(
            "the index cannot use write-ahead logging: {mode}"
        )));
    }
    connection.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
    Ok(connection)
}

/// Remove what the index owns in `dir`: the database and its journal files, and the preview cache.
/// Nothing else in the directory is touched.
fn discard(dir: &Path) -> Result<(), Error> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let path = dir.join(format!("{INDEX_FILE}{suffix}"));
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::file_access(format!(
                    "cannot discard the index: {}",
                    error.kind()
                )));
            }
        }
    }
    match std::fs::remove_dir_all(dir.join(PREVIEWS_DIR)) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::file_access(format!(
            "cannot discard the preview cache: {}",
            error.kind()
        ))),
    }
}

/// Record one file, or replace what the index knew of the file at its path, and answer its row.
/// A replaced row keeps its [`FileId`]; its previews are the preview lane's to check against the
/// new signature.
pub(crate) fn upsert_file(tx: &Transaction<'_>, file: &FileRecord) -> Result<FileId, Error> {
    let identity = file.signature.identity.map(FileIdentity::to_columns);
    let header = file.header.header();
    let capture = header.and_then(|header| header.capture.as_ref());
    let position = header.and_then(|header| header.position.as_ref());
    let camera = header.and_then(|header| header.camera.as_ref());
    let exposure = header.map(|header| header.exposure).unwrap_or_default();
    let dimensions = header.and_then(|header| header.dimensions);
    let thumbnail = header.and_then(|header| header.thumbnail);
    let thumb_size = thumbnail.and_then(|thumb| thumb.size());
    let error = match &file.header {
        HeaderState::Unreadable(error) => Some(error.as_str()),
        _ => None,
    };
    let id = tx.query_row(
        "INSERT INTO files (path, folder, name, volume_id, byte_len, modified_ns, device, inode,
             kind, header_state, header_error, capture_ms, local_text, local_day, offset_minutes,
             latitude, longitude, altitude_m, make, model, body_serial, lens, exposure_time_s,
             f_number, iso, exposure_bias_ev, focal_mm, focal_35mm_mm, width, height, orientation,
             thumb_offset, thumb_len, thumb_format, thumb_width, thumb_height, last_seen_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
             ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35,
             ?36, ?37)
         ON CONFLICT(path) DO UPDATE SET folder = excluded.folder, name = excluded.name,
             volume_id = excluded.volume_id, byte_len = excluded.byte_len,
             modified_ns = excluded.modified_ns, device = excluded.device, inode = excluded.inode,
             kind = excluded.kind, header_state = excluded.header_state,
             header_error = excluded.header_error, capture_ms = excluded.capture_ms,
             local_text = excluded.local_text, local_day = excluded.local_day,
             offset_minutes = excluded.offset_minutes, latitude = excluded.latitude,
             longitude = excluded.longitude, altitude_m = excluded.altitude_m,
             make = excluded.make, model = excluded.model, body_serial = excluded.body_serial,
             lens = excluded.lens, exposure_time_s = excluded.exposure_time_s,
             f_number = excluded.f_number, iso = excluded.iso,
             exposure_bias_ev = excluded.exposure_bias_ev, focal_mm = excluded.focal_mm,
             focal_35mm_mm = excluded.focal_35mm_mm, width = excluded.width,
             height = excluded.height, orientation = excluded.orientation,
             thumb_offset = excluded.thumb_offset, thumb_len = excluded.thumb_len,
             thumb_format = excluded.thumb_format, thumb_width = excluded.thumb_width,
             thumb_height = excluded.thumb_height, last_seen_ms = excluded.last_seen_ms
         RETURNING id",
        params![
            file.path.to_string_lossy(),
            file.folder.to_string_lossy(),
            file.name,
            file.volume_id.as_str(),
            i64::try_from(file.signature.len)
                .map_err(|_| Error::resource_limit("file length exceeds index range"))?,
            file.signature.modified_ns,
            identity.map(|(device, _)| device),
            identity.map(|(_, inode)| inode),
            file.kind.as_str(),
            file.header.as_str(),
            error,
            capture.map(CaptureTime::instant_ms),
            capture.map(|time| time.text.as_str()),
            capture.map(|time| time.local_day().to_string()),
            capture.and_then(|time| time.offset_minutes),
            position.map(|at| at.lat),
            position.map(|at| at.lon),
            position.and_then(|at| at.alt_m),
            camera.map(|body| body.make.as_str()),
            camera.map(|body| body.model.as_str()),
            camera.and_then(|body| body.serial.as_deref()),
            header.and_then(|header| header.lens.as_deref()),
            exposure.time_s,
            exposure.f_number,
            exposure.iso,
            exposure.bias_ev,
            exposure.focal_mm,
            exposure.focal_35mm_mm,
            dimensions.map(|size| size.width),
            dimensions.map(|size| size.height),
            header.and_then(|header| header.orientation.map(ExifOrientation::get)),
            thumbnail
                .map(|thumb| i64::try_from(thumb.offset()))
                .transpose()
                .map_err(|_| Error::resource_limit("thumbnail offset exceeds index range"))?,
            thumbnail.map(|thumb| thumb.len()),
            thumbnail.map(|thumb| thumb.format().as_str()),
            thumb_size.map(|size| size.width),
            thumb_size.map(|size| size.height),
            file.last_seen_ms,
        ],
        |row| row.get(0),
    )?;
    Ok(FileId(id))
}

/// One file's record by its row, or none when the index has no such row. A stored value this build
/// cannot read is refused by name.
pub(crate) fn file(connection: &Connection, id: FileId) -> Result<Option<FileRecord>, Error> {
    let found = connection
        .query_row(
            "SELECT path, folder, name, volume_id, byte_len, modified_ns, device, inode, kind,
                 header_state, header_error, capture_ms, local_text, offset_minutes, latitude,
                 longitude, altitude_m, make, model, body_serial, lens, exposure_time_s, f_number,
                 iso, exposure_bias_ev, focal_mm, focal_35mm_mm, width, height, orientation,
                 thumb_offset, thumb_len, thumb_format, thumb_width, thumb_height, last_seen_ms
             FROM files WHERE id = ?1",
            [id.0],
            |row| {
                Ok(StoredFile {
                    path: row.get(0)?,
                    folder: row.get(1)?,
                    name: row.get(2)?,
                    volume_id: row.get(3)?,
                    byte_len: row.get(4)?,
                    modified_ns: row.get(5)?,
                    device: row.get(6)?,
                    inode: row.get(7)?,
                    kind: row.get(8)?,
                    state: row.get(9)?,
                    error: row.get(10)?,
                    capture_ms: row.get(11)?,
                    local_text: row.get(12)?,
                    offset_minutes: row.get(13)?,
                    position: [row.get(14)?, row.get(15)?, row.get(16)?],
                    camera: [row.get(17)?, row.get(18)?, row.get(19)?],
                    lens: row.get(20)?,
                    exposure: Exposure {
                        time_s: row.get(21)?,
                        f_number: row.get(22)?,
                        iso: row.get(23)?,
                        bias_ev: row.get(24)?,
                        focal_mm: row.get(25)?,
                        focal_35mm_mm: row.get(26)?,
                    },
                    size: [row.get(27)?, row.get(28)?],
                    orientation: row.get(29)?,
                    thumb: (row.get(30)?, row.get(31)?, row.get(32)?),
                    thumb_size: [row.get(33)?, row.get(34)?],
                    last_seen_ms: row.get(35)?,
                })
            },
        )
        .optional()?;
    found.map(StoredFile::into_record).transpose()
}

/// A `files` row as SQLite hands it back, before its values are checked into their types.
struct StoredFile {
    path: String,
    folder: String,
    name: String,
    volume_id: String,
    byte_len: i64,
    modified_ns: i64,
    device: Option<i64>,
    inode: Option<i64>,
    kind: String,
    state: String,
    error: Option<String>,
    capture_ms: Option<i64>,
    local_text: Option<String>,
    offset_minutes: Option<i16>,
    position: [Option<f64>; 3],
    camera: [Option<String>; 3],
    lens: Option<String>,
    exposure: Exposure,
    size: [Option<u32>; 2],
    orientation: Option<u8>,
    thumb: (Option<i64>, Option<u32>, Option<String>),
    thumb_size: [Option<u32>; 2],
    last_seen_ms: i64,
}

impl StoredFile {
    fn into_record(self) -> Result<FileRecord, Error> {
        let bad = |what: &str| Error::incompatible(format!("index file row: {what}"));
        let kind = SourceTag::parse(&self.kind).ok_or_else(|| bad("unknown kind"))?;
        let header = match self.state.as_str() {
            "pending" => HeaderState::Pending,
            "unreadable" => HeaderState::Unreadable(self.error.unwrap_or_default()),
            "ok" => {
                let capture =
                    self.capture_ms
                        .zip(self.local_text)
                        .map(|(instant, text)| CaptureTime {
                            local_ms: instant
                                + i64::from(self.offset_minutes.unwrap_or(0)) * 60_000,
                            offset_minutes: self.offset_minutes,
                            text,
                        });
                let [lat, lon, alt_m] = self.position;
                let [make, model, serial] = self.camera;
                let thumbnail = match self.thumb {
                    (Some(offset), Some(len), Some(format)) => Some(EmbeddedImage::new(
                        u64::try_from(offset).map_err(|_| bad("negative thumbnail offset"))?,
                        len,
                        EmbeddedFormat::parse(&format).ok_or_else(|| bad("thumbnail format"))?,
                        self.thumb_size[0],
                        self.thumb_size[1],
                    )?),
                    _ => None,
                };
                HeaderState::Ok(Box::new(HeaderMetadata {
                    capture,
                    position: lat
                        .zip(lon)
                        .map(|(lat, lon)| GeoPosition { lat, lon, alt_m }),
                    camera: make.zip(model).map(|(make, model)| CameraBody {
                        make,
                        model,
                        serial,
                    }),
                    lens: self.lens,
                    exposure: self.exposure,
                    dimensions: self.size[0]
                        .zip(self.size[1])
                        .map(|(width, height)| Dimensions { width, height }),
                    orientation: self.orientation.and_then(ExifOrientation::new),
                    thumbnail,
                }))
            }
            _ => return Err(bad("unknown header state")),
        };
        Ok(FileRecord {
            path: self.path.into(),
            folder: self.folder.into(),
            name: self.name,
            volume_id: VolumeId::parse(self.volume_id)?,
            signature: FileSignature {
                len: u64::try_from(self.byte_len).map_err(|_| bad("negative length"))?,
                modified_ns: self.modified_ns,
                identity: FileIdentity::from_columns(self.device, self.inode),
            },
            kind,
            header,
            last_seen_ms: self.last_seen_ms,
        })
    }
}

/// Record one root, or replace what the index knew of the root at its path, and answer its row.
pub(crate) fn upsert_root(tx: &Transaction<'_>, root: &IndexRoot) -> Result<i64, Error> {
    Ok(tx.query_row(
        "INSERT INTO roots (path, kind, volume_id, listed_ms, file_count, offline)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(path) DO UPDATE SET kind = excluded.kind, volume_id = excluded.volume_id,
             listed_ms = excluded.listed_ms, file_count = excluded.file_count,
             offline = excluded.offline
         RETURNING id",
        params![
            root.path.to_string_lossy(),
            root.kind.as_str(),
            root.volume_id.as_str(),
            root.listed_ms,
            root.file_count,
            root.offline,
        ],
        |row| row.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog_types::RootKind;
    use luxforge_testbase::paths::temp_dir;

    fn tables(connection: &Connection) -> Vec<String> {
        let mut statement = connection
            .prepare("SELECT name FROM sqlite_schema WHERE type = 'table' ORDER BY name")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn index_format_a_new_index_is_created_versioned_and_reopened() {
        let dir = temp_dir("index-new").join("catalog.index");
        let (index, opened) = IndexDb::open(&dir, "catalog-a").unwrap();
        assert_eq!(opened, IndexOpened::Created);
        assert_eq!(
            tables(index.connection()),
            ["files", "index_meta", "photo_previews", "previews", "roots"]
        );
        let format: i64 = index
            .connection()
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(format, INDEX_FORMAT);
        let worker = index.connect().unwrap();
        drop(index);
        drop(worker);
        let (_, opened) = IndexDb::open(&dir, "catalog-a").unwrap();
        assert_eq!(opened, IndexOpened::Opened);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    /// A mismatched format, a file that is not a database and another catalog's index are each
    /// discarded with the preview cache and recreated empty, and a catalog file beside them keeps
    /// every byte.
    #[test]
    fn index_format_an_unusable_index_is_discarded_without_touching_the_catalog() {
        let root = temp_dir("index-discard");
        let catalog = root.join("catalog.sqlite");
        std::fs::write(&catalog, b"the catalog's own bytes").unwrap();
        let dir = index_dir(&catalog);
        assert_eq!(dir, root.canonicalize().unwrap().join("catalog.index"));
        type Spoil = fn(&Path);
        let spoilers: [(&str, Spoil); 3] = [
            ("index format 7 is not supported", |dir| {
                let connection = Connection::open(dir.join(INDEX_FILE)).unwrap();
                connection.pragma_update(None, "user_version", 7).unwrap();
            }),
            ("not a database", |dir| {
                std::fs::write(dir.join(INDEX_FILE), vec![0x5a; 8192]).unwrap();
                for suffix in ["-wal", "-shm"] {
                    let _ = std::fs::remove_file(dir.join(format!("{INDEX_FILE}{suffix}")));
                }
            }),
            ("another catalog", |_| {}),
        ];
        for (expected, spoil) in spoilers {
            let (index, _) = IndexDb::open(&dir, "catalog-a").unwrap();
            let volume = VolumeId::parse("volume-0123456789").unwrap();
            let record = FileRecord {
                path: "/v/a.jpg".into(),
                folder: "/v".into(),
                name: "a.jpg".into(),
                volume_id: volume,
                signature: FileSignature {
                    len: 1,
                    modified_ns: 2,
                    identity: None,
                },
                kind: SourceTag::Jpeg,
                header: HeaderState::Pending,
                last_seen_ms: 3,
            };
            let mut index = index;
            let tx = index.connection_mut().transaction().unwrap();
            upsert_file(&tx, &record).unwrap();
            tx.commit().unwrap();
            std::fs::create_dir_all(index.previews_dir().join("ab")).unwrap();
            std::fs::write(index.previews_dir().join("ab/c.jpg"), b"jpeg").unwrap();
            drop(index);
            spoil(&dir);
            let catalog_id = if expected == "another catalog" {
                "catalog-b"
            } else {
                "catalog-a"
            };
            let (index, opened) = IndexDb::open(&dir, catalog_id).unwrap();
            match opened {
                IndexOpened::Discarded(reason) => {
                    assert!(reason.contains(expected), "{expected}: {reason}")
                }
                other => panic!("{expected}: {other:?}"),
            }
            let files: i64 = index
                .connection()
                .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
                .unwrap();
            assert_eq!(files, 0, "{expected}: recreated empty");
            assert!(!index.previews_dir().exists(), "{expected}: previews gone");
            drop(index);
            assert_eq!(std::fs::read(&catalog).unwrap(), b"the catalog's own bytes");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn index_format_a_file_row_round_trips_every_header_column() {
        let dir = temp_dir("index-row").join("catalog.index");
        let (mut index, _) = IndexDb::open(&dir, "catalog-a").unwrap();
        let header = HeaderMetadata {
            capture: CaptureTime::from_exif("2026:09:12 10:15:02", Some("130"), Some("+02:00")),
            position: GeoPosition::new(47.66, 9.175, Some(401.5)),
            camera: Some(CameraBody {
                make: "NIKON CORPORATION".into(),
                model: "NIKON Z 8".into(),
                serial: Some("3001234".into()),
            }),
            lens: Some("NIKKOR Z 24-120mm f/4 S".into()),
            exposure: Exposure {
                time_s: Some(0.004),
                f_number: Some(8.0),
                iso: Some(64),
                bias_ev: Some(-0.7),
                focal_mm: Some(35.0),
                focal_35mm_mm: Some(35.0),
            },
            dimensions: Some(Dimensions {
                width: 8256,
                height: 5504,
            }),
            orientation: ExifOrientation::new(6),
            thumbnail: Some(
                EmbeddedImage::new(
                    1024,
                    160 * 120 * 3,
                    EmbeddedFormat::Rgb8,
                    Some(160),
                    Some(120),
                )
                .unwrap(),
            ),
        };
        let record = FileRecord {
            path: "/Volumes/NIKON Z 8/DCIM/100NZ8_1/DSC_0001.NEF".into(),
            folder: "/Volumes/NIKON Z 8/DCIM/100NZ8_1".into(),
            name: "DSC_0001.NEF".into(),
            volume_id: VolumeId::parse("volume-card0000001").unwrap(),
            signature: FileSignature {
                len: 45_000_000,
                modified_ns: 1_789_200_902_130_000_000,
                identity: Some(FileIdentity {
                    device: u64::MAX - 1,
                    inode: 42,
                }),
            },
            kind: SourceTag::Raw,
            header: HeaderState::Ok(Box::new(header)),
            last_seen_ms: 1_789_200_999_000,
        };
        let tx = index.connection_mut().transaction().unwrap();
        let id = upsert_file(&tx, &record).unwrap();
        let root = IndexRoot {
            path: "/Volumes/NIKON Z 8".into(),
            kind: RootKind::Card,
            volume_id: record.volume_id.clone(),
            listed_ms: Some(5),
            file_count: Some(1),
            offline: false,
        };
        let root_id = upsert_root(&tx, &root).unwrap();
        assert_eq!(
            upsert_root(&tx, &root).unwrap(),
            root_id,
            "a root keeps its row"
        );
        tx.commit().unwrap();
        assert_eq!(
            file(index.connection(), id).unwrap().as_ref(),
            Some(&record)
        );
        let unreadable = FileRecord {
            header: HeaderState::Unreadable("truncated TIFF header".into()),
            ..record.clone()
        };
        let tx = index.connection_mut().transaction().unwrap();
        assert_eq!(
            upsert_file(&tx, &unreadable).unwrap(),
            id,
            "a file keeps its row"
        );
        tx.commit().unwrap();
        assert_eq!(file(index.connection(), id).unwrap(), Some(unreadable));
        assert_eq!(file(index.connection(), FileId(id.0 + 1)).unwrap(), None);
        drop(index);
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
