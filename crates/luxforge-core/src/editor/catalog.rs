use super::{ActionResult, AssetRecord, EditorService, artifact_store, entries::Head};
use crate::{AssetId, EntryId, Error, ErrorKind, HistoryEntry, Recipe};
use rusqlite::{
    Connection, ErrorCode, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Format 11 stores each asset's source kind tag in a column of its own beside the interpretation,
/// so a `catalog.list` page reads columns only and decodes no interpretation. Format 10 stored each
/// asset request's whole answer in the request table — for a `mask.*` command
/// the label it committed and the mask and component it addressed or minted beside the mutation
/// result — so a retry answers with the identities the first attempt created. Format 9 kept each
/// entry's history row — its label, actor, timestamp and restore target, beside the sequence, action
/// and undo parent format 7 already held — in the entry's own columns, so a page of history rows
/// decodes no entry. Format 7 was the merged shape: the mask table a recipe
/// carries and the layer's mask reference, the content-addressed stroke store a painted path is
/// kept in — so no catalog ever holds embedded stroke points — the preset library, and the
/// catalog's own identity with the derived-artifact tables. Format 4 made entry records the only
/// stored copy of a stack and format 3 stored each entry's rendered label. Every other marker,
/// earlier or later, is refused by name and left as it is; choose a new catalog path.
pub(super) const CATALOG_FORMAT: i64 = 11;
pub(super) const ASSET_COLUMNS: &str =
    "id,source_root,locator,fingerprint,file_identity,byte_len,width,height,source_json";

/// Assets per `catalog.list` page, and the page a request that names no `limit` gets. A page reads
/// `limit + 1` rows of the asset table's own columns and decodes nothing, so its cost is bounded by
/// the limit however large the catalog is.
pub(crate) const MAX_ASSET_PAGE: usize = 500;
pub(crate) const DEFAULT_ASSET_PAGE: usize = 100;

/// A catalog failure: `conflict` while another connection holds the database, `catalog` otherwise.
impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        let kind = match &error {
            rusqlite::Error::SqliteFailure(problem, _)
                if matches!(
                    problem.code,
                    ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked
                ) =>
            {
                ErrorKind::Conflict
            }
            _ => ErrorKind::Catalog,
        };
        Error::new(kind, error.to_string())
    }
}

/// Run `f` in one `BEGIN IMMEDIATE` transaction and commit what it wrote, or write nothing: an
/// error from `f` or from the commit rolls the whole transaction back. Every catalog write goes
/// through here — history through [`EditorService::mutate`], an import, versions, the preset
/// library and the artifact rows — so each one is short and atomic.
///
/// It takes the connection rather than the service, so the caller's closure can still borrow the
/// service's other fields, such as the artifact root an entry's references are checked in.
pub(crate) fn write<T>(
    connection: &mut Connection,
    f: impl FnOnce(&Transaction<'_>) -> Result<T, Error>,
) -> Result<T, Error> {
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let value = f(&tx)?;
    tx.commit()?;
    Ok(value)
}

pub(super) fn json_error(context: &str, error: impl std::fmt::Display) -> Error {
    Error::incompatible(format!("{context}: {error}"))
}

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<String, Error> {
    serde_json::to_string(value).map_err(|e| json_error("cannot encode catalog value", e))
}

pub(crate) fn decode<T: DeserializeOwned>(context: &str, value: String) -> Result<T, Error> {
    #[cfg(test)]
    super::read_counts::decoded();
    serde_json::from_str(&value).map_err(|e| json_error(context, e))
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}

/// Take the catalog for `connection` alone and answer its format marker, before anything is
/// written. The exclusive locking mode comes before any read, so in WAL SQLite keeps the log's
/// index in this process's memory and never creates a `<catalog>-shm` file, and the empty
/// transaction takes the lock now, so a second owner is refused here rather than at its first
/// write.
pub(super) fn lock(connection: &Connection) -> Result<i64, Error> {
    connection.execute_batch(
        "PRAGMA foreign_keys=ON;
         PRAGMA locking_mode=EXCLUSIVE;
         BEGIN IMMEDIATE;
         COMMIT;",
    )?;
    Ok(connection.pragma_query_value(None, "user_version", |row| row.get(0))?)
}

/// Refuse an unmarked database that holds anything: only an empty one becomes a catalog.
pub(super) fn require_empty(connection: &Connection) -> Result<(), Error> {
    let occupied: bool =
        connection.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema)", [], |row| {
            row.get(0)
        })?;
    if occupied {
        return Err(Error::incompatible(
            "unmarked catalog is not empty; choose a new catalog path",
        ));
    }
    Ok(())
}

/// Set the journal of a catalog [`lock`] has taken and this build opens: a write-ahead log, which
/// a commit appends to once, flushed in full at every commit and checkpoint when `durable` —
/// `F_FULLFSYNC` on macOS, as [`crate::atomic_file::flush`] makes every other durable write.
/// `durable` is [`crate::atomic_file::FLUSHES`]: a test build keeps the same log, its file and its
/// recovery, and skips only the flush. The flush settings come first, so the one write that moves
/// a catalog from a rollback journal is flushed too. A catalog SQLite cannot keep a log for is
/// refused, never opened on a weaker journal.
pub(super) fn configure(connection: &Connection, durable: bool) -> Result<(), Error> {
    connection.execute_batch(if durable {
        "PRAGMA synchronous=FULL;
         PRAGMA fullfsync=ON;
         PRAGMA checkpoint_fullfsync=ON;"
    } else {
        "PRAGMA synchronous=OFF;"
    })?;
    let journal: String =
        connection.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
    if journal != "wal" {
        return Err(Error::catalog(format!(
            "catalog cannot keep a write-ahead log (SQLite kept its {journal} journal); \
             choose a catalog file on a local disk"
        )));
    }
    Ok(())
}

impl EditorService {
    /// Create the current schema in an empty catalog, [`require_empty`] having checked it is.
    pub(super) fn create_schema(connection: &mut Connection) -> Result<(), Error> {
        write(connection, |tx| {
            Ok(tx.execute_batch(&format!(
                "CREATE TABLE assets (
                    id TEXT PRIMARY KEY,
                    source_root TEXT NOT NULL,
                    locator TEXT NOT NULL,
                    canonical_locator TEXT NOT NULL UNIQUE,
                    file_identity TEXT NOT NULL UNIQUE,
                    fingerprint TEXT NOT NULL,
                    byte_len INTEGER NOT NULL,
                    width INTEGER NOT NULL,
                    height INTEGER NOT NULL,
                    source_json TEXT NOT NULL,
                    source_kind TEXT NOT NULL
                 );
                 CREATE TABLE entries (
                    id TEXT PRIMARY KEY,
                    asset_id TEXT NOT NULL REFERENCES assets(id),
                    sequence INTEGER NOT NULL,
                    action_id TEXT NOT NULL,
                    label TEXT NOT NULL,
                    actor TEXT NOT NULL,
                    timestamp_ms INTEGER NOT NULL,
                    undo_parent_id TEXT,
                    restore_target_id TEXT,
                    entry_json TEXT NOT NULL,
                    UNIQUE(asset_id, sequence)
                 );
                 CREATE TABLE asset_state (
                    asset_id TEXT PRIMARY KEY REFERENCES assets(id),
                    current_entry_id TEXT NOT NULL REFERENCES entries(id),
                    revision INTEGER NOT NULL,
                    redo_json TEXT NOT NULL
                 );
                 CREATE TABLE requests (
                    asset_id TEXT NOT NULL REFERENCES assets(id),
                    request_id TEXT NOT NULL,
                    input_hash TEXT NOT NULL,
                    result_json TEXT NOT NULL,
                    PRIMARY KEY(asset_id, request_id)
                 );
                 CREATE TABLE versions (
                    asset_id TEXT NOT NULL REFERENCES assets(id),
                    name TEXT NOT NULL COLLATE NOCASE,
                    entry_id TEXT NOT NULL REFERENCES entries(id),
                    actor TEXT NOT NULL,
                    created_ms INTEGER NOT NULL,
                    PRIMARY KEY(asset_id, name)
                 );
                 CREATE TABLE presets (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL COLLATE NOCASE,
                    group_name TEXT NOT NULL COLLATE NOCASE,
                    record_json TEXT NOT NULL,
                    source_text TEXT,
                    UNIQUE(group_name, name)
                 );
                 CREATE TABLE strokes (
                    id TEXT PRIMARY KEY,
                    stroke_json TEXT NOT NULL
                 );
                 CREATE TRIGGER strokes_are_immutable BEFORE UPDATE ON strokes BEGIN
                    SELECT RAISE(ABORT, 'stored strokes are immutable');
                 END;
                 CREATE TRIGGER entries_are_immutable BEFORE UPDATE ON entries BEGIN
                    SELECT RAISE(ABORT, 'history entries are immutable');
                 END;
                 CREATE TABLE catalog_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                 );
                 CREATE TABLE artifacts (
                    id TEXT PRIMARY KEY,
                    sha256 TEXT NOT NULL,
                    bytes INTEGER NOT NULL,
                    kind TEXT NOT NULL,
                    width INTEGER,
                    height INTEGER,
                    colour TEXT,
                    module_id TEXT NOT NULL,
                    created_ms INTEGER NOT NULL
                 );
                 CREATE TABLE artifact_refs (
                    entry_id TEXT NOT NULL REFERENCES entries(id),
                    artifact_id TEXT NOT NULL REFERENCES artifacts(id),
                    PRIMARY KEY(entry_id, artifact_id)
                 );
                 CREATE INDEX artifact_refs_by_artifact ON artifact_refs(artifact_id);
                 CREATE TRIGGER artifact_refs_are_permanent BEFORE DELETE ON artifact_refs BEGIN
                    SELECT RAISE(ABORT, 'artifact references are permanent');
                 END;
                 INSERT INTO catalog_meta VALUES ('catalog_id', '{catalog_id}');
                 PRAGMA user_version={CATALOG_FORMAT};",
                catalog_id = uuid::Uuid::new_v4()
            ))?)
        })
    }
}

pub(super) fn input_hash(input: &Value) -> Result<String, Error> {
    Ok(format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(input).map_err(|e| json_error("cannot encode request", e))?
        )
    ))
}

/// Record one mutation's result under its request identity, in the transaction that made the
/// mutation, so a retry is answered from here. A row already under that identity means the same
/// request was committed since this one looked it up: that is a `conflict`, whatever the mutation,
/// and the caller's whole transaction rolls back. The insert itself is the check, so nothing can
/// commit the request between a check and the row.
pub(super) fn insert_request(
    tx: &Transaction<'_>,
    asset_id: &AssetId,
    request_id: &str,
    input: &Value,
    result: &ActionResult,
) -> Result<(), Error> {
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO requests VALUES (?1,?2,?3,?4)",
        params![
            asset_id.as_str(),
            request_id,
            input_hash(input)?,
            encode(result)?
        ],
    )?;
    if inserted == 0 {
        return Err(Error::conflict("request was committed concurrently"));
    }
    Ok(())
}

/// The artifact directory a catalog uses: `<stem>.artifacts` beside the catalog file. One rule, so
/// anything writing an entry without an open service — a test on the production write path —
/// names the same directory the service would.
pub(super) fn default_artifact_root(catalog: &Path) -> PathBuf {
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
        .join(format!("{stem}.artifacts"))
}

/// Write one history entry in the caller's transaction: its strokes to the store, its row and its
/// artifact references. Every path that writes an entry comes through here, so the references are
/// checked and recorded with the entry or not at all: a snapshot can never point at an artifact the
/// catalog does not hold.
///
/// The fields of the entry's [`HistoryRow`](crate::HistoryRow) are written to their own columns from the same entry,
/// beside its JSON, so a history page reads them without decoding it. Neither ever changes.
///
/// It writes and does not validate. The caller [admitted](EditorService::admit) the stack before it
/// opened the transaction, and that is the one validation a commit makes.
pub(super) fn insert_entry(
    tx: &Transaction<'_>,
    artifact_root: &Path,
    entry: &HistoryEntry,
) -> Result<(), Error> {
    store_strokes(tx, &entry.snapshot.recipe)?;
    tx.execute(
        "INSERT INTO entries (id,asset_id,sequence,action_id,label,actor,timestamp_ms,
                              undo_parent_id,restore_target_id,entry_json)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
        params![
            entry.id.as_str(),
            entry.asset_id.as_str(),
            entry.sequence as i64,
            entry.action_id,
            entry.label,
            entry.actor,
            entry.timestamp_ms,
            entry.undo_parent.as_ref().map(EntryId::as_str),
            entry.restore_target.as_ref().map(EntryId::as_str),
            encode(entry)?
        ],
    )?;
    artifact_store::link_artifacts(tx, artifact_root, entry)
}

/// Write this recipe's **fresh** strokes to the content-addressed store: the ones its table does
/// not already know are durable there.
///
/// The address is the content's, so a stroke a later entry references again is already there and
/// the insert does nothing: that is the whole of "stored once", and it needs no reference count and
/// no check of what else points at it. The entry's own JSON carries only the addresses, so nothing
/// written here is ever written into an entry.
///
/// Writing every reference instead would still be correct, since `INSERT OR IGNORE` makes a repeat
/// a no-op, but it would cost `O(references)` SQL per commit and `O(n²)` over a painting session.
/// `recipe.strokes` answers "is this one already durable" for nothing: hydrating a recipe out of
/// the catalog marks every stroke it resolves, and nothing else populates the table except a fresh
/// insert this command made, so writing only the references it does not know are stored is exact,
/// not a heuristic: see [`crate::path::StrokeTable`].
///
/// A reference the recipe could not resolve writes nothing and is not an error at this boundary: an
/// unresolvable reference is retained data, and the paths that would *draw* it refuse it by name. A
/// reference repeated within the one recipe — two components sharing a stroke, or a duplicated mask
/// sitting beside the mask it copied — is written at most once here too.
fn store_strokes(tx: &Transaction<'_>, recipe: &Recipe) -> Result<(), Error> {
    let references = recipe.stroke_references()?;
    write_fresh_strokes(tx, &references, &recipe.strokes)
}

/// [`store_strokes`] over a list of references and the table they resolve in, whichever consumer's
/// strokes they are: the table holds each as its consumer declared it and hands back its canonical
/// bytes, which are what is stored, so the rule — write only what is not known stored, and each
/// address once — is the store's and not any one consumer's.
fn write_fresh_strokes(
    tx: &Transaction<'_>,
    references: &[crate::path::StrokeReference],
    strokes: &crate::path::StrokeTable,
) -> Result<(), Error> {
    if references.is_empty() {
        return Ok(());
    }
    let mut statement =
        tx.prepare("INSERT OR IGNORE INTO strokes (id,stroke_json) VALUES (?1,?2)")?;
    let mut issued = std::collections::BTreeSet::new();
    for reference in references {
        let id = &reference.id;
        if strokes.is_known_stored(id) || !issued.insert(id.clone()) {
            continue;
        }
        let Some(bytes) = strokes.stored_bytes(id) else {
            continue;
        };
        let text = String::from_utf8(bytes)
            .map_err(|e| Error::internal(format!("cannot store stroke: {e}")))?;
        statement.execute(params![id.as_str(), text])?;
        #[cfg(test)]
        super::stroke_writes::written();
    }
    Ok(())
}

/// Resolve this recipe's stroke references against the store, one lookup and one hash each.
///
/// Nothing is replayed and no earlier entry is read: an entry is a complete snapshot, and this is
/// the lookup that turns its addresses back into the strokes they name. A reference the store does
/// not hold, or whose stored bytes are not the bytes the address names, is recorded as a fault
/// rather than raised here, so reading, listing, undoing and carrying the stack forward keep
/// working; the refusal happens where the recipe is compiled, which is every path that would draw
/// it.
///
/// Every stroke this resolves is durable by construction — it was just read from the store — so it
/// goes in through [`crate::path::StrokeTable::load`] as the stroke type its reference declares,
/// which trusts the address that type's `from_stored` already checked the stored bytes against, and
/// marks it known stored: a later commit built on this recipe writes only what it captures fresh,
/// never these.
fn hydrate_strokes(
    connection: &Connection,
    recipe: &mut Recipe,
    origin: &str,
) -> Result<(), Error> {
    let references = recipe.stroke_references()?;
    if references.is_empty() {
        return Ok(());
    }
    recipe.strokes = read_strokes(connection, references, origin)?;
    Ok(())
}

/// [`hydrate_strokes`] over a list of references, whichever consumer's strokes they are: each
/// address is read once and decoded as the type its reference declares.
fn read_strokes(
    connection: &Connection,
    references: Vec<crate::path::StrokeReference>,
    origin: &str,
) -> Result<crate::path::StrokeTable, Error> {
    let mut table = crate::path::StrokeTable::new(origin);
    let mut statement = connection.prepare("SELECT stroke_json FROM strokes WHERE id=?1")?;
    for reference in references {
        if table.knows(&reference.id) {
            continue;
        }
        let stored: Option<String> = statement
            .query_row(params![reference.id.as_str()], |row| row.get(0))
            .optional()?;
        table.load(
            reference.id,
            reference.kind,
            stored.as_deref().map(str::as_bytes),
        );
    }
    Ok(table)
}

pub(super) fn next_sequence(connection: &Connection, asset_id: &AssetId) -> Result<u64, Error> {
    let sequence: i64 = connection.query_row(
        "SELECT COALESCE(MAX(sequence),-1)+1 FROM entries WHERE asset_id=?1",
        [asset_id.as_str()],
        |row| row.get(0),
    )?;
    u64::try_from(sequence).map_err(|_| Error::catalog("invalid history sequence"))
}

/// One asset row, with its source interpretation still the text the catalog holds.
pub(super) struct AssetRow {
    id: AssetId,
    source_root: String,
    locator: String,
    fingerprint: String,
    file_identity: String,
    byte_len: i64,
    width: i64,
    height: i64,
    source: String,
}

pub(super) fn asset_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRow> {
    let id = AssetId::parse(row.get::<_, String>(0)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(AssetRow {
        id,
        source_root: row.get(1)?,
        locator: row.get(2)?,
        fingerprint: row.get(3)?,
        file_identity: row.get(4)?,
        byte_len: row.get(5)?,
        width: row.get(6)?,
        height: row.get(7)?,
        source: row.get(8)?,
    })
}

impl AssetRow {
    /// The record, with its source interpretation parsed into its type and checked — a RAW
    /// interpretation's correction record against its mode — once, here, for this row read. One
    /// this build cannot read is refused by name and left as it is stored.
    pub(super) fn into_record(self) -> Result<AssetRecord, Error> {
        Ok(AssetRecord {
            id: self.id,
            source_root: PathBuf::from(self.source_root),
            locator: PathBuf::from(self.locator),
            fingerprint: self.fingerprint,
            file_identity: self.file_identity,
            byte_len: self.byte_len as u64,
            width: self.width as u32,
            height: self.height as u32,
            source: decode("stored source interpretation", self.source)?,
        })
    }
}

/// One asset's head as its rows hold it: the asset, parsed once, and where its history stands.
pub(super) fn head_from(connection: &Connection, asset_id: &AssetId) -> Result<Head, Error> {
    let asset = connection
        .query_row(
            &format!("SELECT {ASSET_COLUMNS} FROM assets WHERE id=?1"),
            [asset_id.as_str()],
            asset_row,
        )
        .optional()?
        .ok_or_else(|| Error::validation("unknown asset"))?
        .into_record()?;
    let (current, revision, redo): (String, i64, String) = connection.query_row(
        "SELECT current_entry_id,revision,redo_json FROM asset_state WHERE asset_id=?1",
        [asset_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    Ok(Head {
        asset,
        revision: revision as u64,
        current: EntryId::parse(current)?,
        redo: decode("invalid redo state", redo)?,
    })
}

/// The asset's revision from its one integer column, decoding nothing.
pub(super) fn stored_revision(connection: &Connection, asset_id: &AssetId) -> Result<u64, Error> {
    connection
        .query_row(
            "SELECT revision FROM asset_state WHERE asset_id=?1",
            [asset_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .map(|revision| revision as u64)
        .ok_or_else(|| Error::validation("unknown asset"))
}

pub(super) fn entry_from(
    connection: &Connection,
    asset_id: &AssetId,
    entry_id: &EntryId,
) -> Result<HistoryEntry, Error> {
    let json: String = connection
        .query_row(
            "SELECT entry_json FROM entries WHERE id=?1 AND asset_id=?2",
            params![entry_id.as_str(), asset_id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::validation("history entry does not belong to this asset"))?;
    let mut entry: HistoryEntry = decode("invalid history entry", json)?;
    // One lookup per referenced stroke, here and nowhere else: every path that evaluates an entry —
    // state, preview, undo, redo, Restore, export — reads it through this function, once, and then
    // from the owner's entry cache; a listing, which never draws anything, keeps the stored
    // addresses and pays nothing.
    let origin = format!("entry {}", entry.id);
    hydrate_strokes(connection, &mut entry.snapshot.recipe, &origin)?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::test_support::{
        brushed, commit, fixture, mutation, next_entry, stored_entry_json, stroke, temp,
    };
    use crate::{
        Component, ComponentMode, Draft, Mask, ModuleRegistry, Snapshot, SnapshotId,
        mask::commands::{self, MaskTarget},
    };
    use serde_json::{Value, json};

    /// A page of assets reads the asset table's own columns: it decodes no stored value, whatever
    /// the interpretations hold, and its kinds are the tags imported beside them.
    #[test]
    fn an_asset_page_decodes_nothing() {
        let catalog = temp("asset-page.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset;
        crate::editor::read_counts::take();
        let page = service.assets(None, MAX_ASSET_PAGE).unwrap();
        assert_eq!(crate::editor::read_counts::take(), (0, 0));
        assert_eq!(
            page.assets,
            [crate::AssetSummary {
                id: asset.id.clone(),
                locator: asset.locator.clone(),
                kind: asset.source.tag(),
                width: asset.width,
                height: asset.height,
            }]
        );
        assert_eq!(page.next, None);
        assert!(service.assets(None, 0).is_err());
        assert!(service.assets(None, MAX_ASSET_PAGE + 1).is_err());
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn competing_catalog_owners_are_rejected() {
        let catalog = temp("owner.sqlite");
        let owner = EditorService::open(&catalog).unwrap();
        assert_eq!(
            EditorService::open(&catalog).unwrap_err().kind,
            ErrorKind::Conflict
        );
        drop(owner);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn unsupported_catalog_formats_are_rejected_without_rewriting_data() {
        for marker in [0, CATALOG_FORMAT - 1, CATALOG_FORMAT + 1] {
            let catalog = temp("unsupported-format.sqlite");
            let mut service = EditorService::open(&catalog).unwrap();
            service.import(&fixture()).unwrap();
            drop(service);
            let connection = Connection::open(&catalog).unwrap();
            connection
                .pragma_update(None, "user_version", marker)
                .unwrap();
            drop(connection);
            let before = std::fs::read(&catalog).unwrap();
            let error = EditorService::open(&catalog).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Incompatible);
            assert!(error.detail.contains("choose a new catalog path"));
            // The predecessor format is refused by name like any other: its stacks carry no mask
            // table, and a marker that is only one behind is not a reason to guess at one.
            if marker != 0 {
                assert_eq!(
                    error.detail,
                    format!(
                        "catalog format {marker} is not supported; expected {CATALOG_FORMAT}; \
                         choose a new catalog path"
                    )
                );
            }
            assert_eq!(
                std::fs::read(&catalog).unwrap(),
                before,
                "the refused catalog keeps every byte"
            );
            assert!(
                !default_artifact_root(&catalog).exists(),
                "a refused catalog gets no artifact directory"
            );
            std::fs::remove_file(catalog).unwrap();
        }
    }

    /// A connection's journal: its mode, `synchronous`, `fullfsync`, `checkpoint_fullfsync` and
    /// locking mode.
    fn journal(connection: &Connection) -> (String, i64, i64, i64, String) {
        let text = |name| {
            connection
                .pragma_query_value(None, name, |row| row.get::<_, String>(0))
                .unwrap()
        };
        let number = |name| {
            connection
                .pragma_query_value(None, name, |row| row.get::<_, i64>(0))
                .unwrap()
        };
        (
            text("journal_mode"),
            number("synchronous"),
            number("fullfsync"),
            number("checkpoint_fullfsync"),
            text("locking_mode"),
        )
    }

    /// A durable catalog — every build's but a test build's — opens in a write-ahead log flushed in
    /// full at every commit and checkpoint, under the exclusive lock that keeps the log's index in
    /// memory, so only the log is beside it. Every test build skips the flush, so the durable
    /// settings are read through the function that sets them, on a catalog the test build made in
    /// the same log without them.
    #[test]
    fn a_durable_catalog_opens_in_wal_with_full_flushes() {
        let catalog = temp("durable.sqlite");
        let service = EditorService::open(&catalog).unwrap();
        assert_eq!(
            journal(&service.connection),
            ("wal".into(), 0, 0, 0, "exclusive".into()),
            "a test build keeps the same log and skips only the flush"
        );
        assert!(luxforge_testbase::paths::wal(&catalog).exists());
        assert!(!luxforge_testbase::paths::shm(&catalog).exists());
        drop(service);
        let connection = Connection::open(&catalog).unwrap();
        assert_eq!(lock(&connection).unwrap(), CATALOG_FORMAT);
        configure(&connection, true).unwrap();
        assert_eq!(
            journal(&connection),
            ("wal".into(), 2, 1, 1, "exclusive".into())
        );
        assert!(
            luxforge_testbase::paths::wal(&catalog).exists(),
            "the log is beside the open catalog"
        );
        assert!(
            !luxforge_testbase::paths::shm(&catalog).exists(),
            "and no shared-memory file is"
        );
        drop(connection);
        assert!(!luxforge_testbase::paths::wal(&catalog).exists());
        std::fs::remove_file(catalog).unwrap();
    }

    /// A catalog SQLite cannot keep a write-ahead log for, such as one in memory, is refused rather
    /// than opened on a weaker journal.
    #[test]
    fn a_catalog_without_a_write_ahead_log_is_refused() {
        let error = EditorService::open(Path::new(":memory:")).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Catalog);
        assert_eq!(
            error.detail,
            "catalog cannot keep a write-ahead log (SQLite kept its memory journal); \
             choose a catalog file on a local disk"
        );
    }

    /// A file refused at open keeps every byte, its journal mode included: a file on a rollback
    /// journal — an unsupported catalog, or a database that is not one — is refused before the
    /// journal is set, so it is not moved to the log.
    #[test]
    fn a_refused_file_keeps_its_rollback_journal_and_every_byte() {
        for marker in [0, CATALOG_FORMAT + 1] {
            let catalog = temp("refused-rollback.sqlite");
            let mut service = EditorService::open(&catalog).unwrap();
            service.import(&fixture()).unwrap();
            drop(service);
            let connection = Connection::open(&catalog).unwrap();
            let journal: String = connection
                .pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))
                .unwrap();
            assert_eq!(journal, "delete");
            connection
                .pragma_update(None, "user_version", marker)
                .unwrap();
            drop(connection);
            let before = std::fs::read(&catalog).unwrap();
            let error = EditorService::open(&catalog).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Incompatible);
            assert_eq!(
                std::fs::read(&catalog).unwrap(),
                before,
                "the refused file keeps every byte"
            );
            assert!(!luxforge_testbase::paths::wal(&catalog).exists());
            assert_eq!(
                Connection::open(&catalog)
                    .unwrap()
                    .pragma_query_value(None, "journal_mode", |row| row.get::<_, String>(0))
                    .unwrap(),
                "delete"
            );
            std::fs::remove_file(catalog).unwrap();
        }
    }

    /// Commits enough to read back: an import, three edits, an undo and a version.
    fn committed(service: &mut EditorService) -> AssetId {
        let asset = service.import(&fixture()).unwrap().asset.id;
        for (revision, rgb) in [[1, 2, 3], [4, 5, 6], [7, 8, 9]].into_iter().enumerate() {
            let revision = revision as u64;
            service
                .apply_pixel(
                    &asset,
                    mutation(revision, &format!("edit-{revision}")),
                    0,
                    0,
                    rgb,
                )
                .unwrap();
        }
        service.undo(&asset, mutation(3, "undo")).unwrap();
        service
            .create_version(&asset, "Kept", None, "test")
            .unwrap();
        asset
    }

    /// What a reopen must read again: the head, the history rows and the versions.
    fn read_back(
        service: &EditorService,
        asset: &AssetId,
    ) -> (crate::EditorState, crate::HistoryPage, Vec<crate::Version>) {
        let read = (
            service.state(asset).unwrap(),
            service.history(asset, None, 10).unwrap(),
            service.versions(asset).unwrap(),
        );
        assert_eq!(read.1.entries.len(), 4, "the import and three edits");
        assert_eq!(read.2.len(), 1);
        read
    }

    /// A crash leaves the log beside the catalog, holding commits the catalog file does not yet
    /// have. What a process that never closed its catalog leaves — the catalog file and its log,
    /// copied while the owner still holds them — reopens with every committed entry, the head and
    /// every history row and version.
    #[test]
    fn a_catalog_reopened_after_a_crash_recovers_every_commit_from_its_log() {
        let original = luxforge_testbase::paths::temp_dir("crashed-from");
        let catalog = original.join("catalog.sqlite");
        let developer = || std::sync::Arc::new(ModuleRegistry::developer());
        let mut service = EditorService::open_with(&catalog, developer()).unwrap();
        let asset = committed(&mut service);
        let before = read_back(&service, &asset);
        let wal = luxforge_testbase::paths::wal(&catalog);
        assert!(
            wal.metadata().unwrap().len() > 32,
            "the commits are in the log, past its header"
        );
        assert!(!luxforge_testbase::paths::shm(&catalog).exists());
        // Read and written rather than copied, so no platform's copy call refuses a file another
        // handle has open for writing.
        let (file, log) = (
            std::fs::read(&catalog).unwrap(),
            std::fs::read(&wal).unwrap(),
        );
        let crashed = luxforge_testbase::paths::temp_dir("crashed-to");
        let alone = crashed.join("alone.sqlite");
        std::fs::write(&alone, &file).unwrap();
        let tables: i64 = Connection::open(&alone)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM sqlite_schema", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tables, 0, "the catalog file alone holds none of it");
        let copy = crashed.join("catalog.sqlite");
        std::fs::write(&copy, &file).unwrap();
        std::fs::write(luxforge_testbase::paths::wal(&copy), &log).unwrap();
        let recovered = EditorService::open_with(&copy, developer()).unwrap();
        assert_eq!(read_back(&recovered, &asset), before);
        drop(recovered);
        assert!(
            !luxforge_testbase::paths::wal(&copy).exists(),
            "the recovered catalog closes clean"
        );
        drop(service);
        std::fs::remove_dir_all(original).unwrap();
        std::fs::remove_dir_all(crashed).unwrap();
    }

    /// A clean close checkpoints the log into the catalog and removes it, so a closed catalog is the
    /// one file, and a reopen reads every commit from it.
    #[test]
    fn a_clean_close_leaves_the_catalog_one_file_holding_every_commit() {
        let directory = luxforge_testbase::paths::temp_dir("closed");
        let catalog = directory.join("catalog.sqlite");
        let developer = || std::sync::Arc::new(ModuleRegistry::developer());
        let mut service = EditorService::open_with(&catalog, developer()).unwrap();
        let asset = committed(&mut service);
        let before = read_back(&service, &asset);
        drop(service);
        assert!(!luxforge_testbase::paths::wal(&catalog).exists());
        assert!(!luxforge_testbase::paths::shm(&catalog).exists());
        let reopened = EditorService::open_with(&catalog, developer()).unwrap();
        assert_eq!(read_back(&reopened, &asset), before);
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn stored_strokes(catalog: &Path) -> i64 {
        Connection::open(catalog)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM strokes", [], |row| row.get(0))
            .unwrap()
    }

    /// A stroke is stored once under its address however many entries reference it, and an entry
    /// holds addresses and no positions at all.
    #[test]
    fn one_stroke_is_stored_once_and_referenced_from_every_entry_that_holds_it() {
        let catalog = temp("stroke-store.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let shared = stroke(1);
        let first = next_entry(
            &state,
            brushed(
                &state.current_entry.snapshot.recipe,
                std::slice::from_ref(&shared),
            ),
        );
        drop(service);
        commit(&catalog, &first);

        let service = EditorService::open(&catalog).unwrap();
        let state = service.state(&asset).unwrap();
        // A second entry drawn on top: the same stroke again, plus one more.
        let second = next_entry(
            &state,
            brushed(
                &state.current_entry.snapshot.recipe,
                &[shared.clone(), stroke(2)],
            ),
        );
        drop(service);
        commit(&catalog, &second);

        assert_eq!(
            stored_strokes(&catalog),
            2,
            "the shared stroke is one stored object, not one per entry"
        );
        // Neither entry's JSON holds a position: the addresses are there and the points are not.
        for entry in [&first, &second] {
            let json = stored_entry_json(&catalog, &entry.id);
            assert!(
                json.contains(shared.id().as_str()),
                "an entry references the stroke by address"
            );
            assert!(
                !json.contains(r#""points""#),
                "no catalog holds embedded stroke positions"
            );
        }

        // And reading an entry back resolves what it references, without replaying anything.
        let service = EditorService::open(&catalog).unwrap();
        let read = service.entry(&asset, &second.id).unwrap();
        assert_eq!(
            read.snapshot.recipe.strokes.get(&shared.id()),
            Some(&shared)
        );
        assert_eq!(
            read.snapshot.recipe.strokes.strokes().count(),
            2,
            "one resolved stroke per distinct address"
        );
        // The earlier entry is still its own complete snapshot and resolves on its own.
        let earlier = service.entry(&asset, &first.id).unwrap();
        assert_eq!(earlier.snapshot.recipe.strokes.strokes().count(), 1);
        assert_eq!(
            earlier.snapshot.recipe.masks[0].components[0].payload,
            first.snapshot.recipe.masks[0].components[0].payload,
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A referenced stroke that is gone, or whose stored bytes are not the bytes its address names,
    /// refuses every path that would draw the recipe and keeps everything it has.
    #[test]
    fn a_missing_or_corrupt_stroke_refuses_every_path_that_would_draw_it() {
        for what in [false, true] {
            let catalog = temp("broken-stroke.sqlite");
            let mut service = EditorService::open(&catalog).unwrap();
            let asset = service.import(&fixture()).unwrap().asset.id;
            let state = service.state(&asset).unwrap();
            let drawn = stroke(3);
            let mut painted = brushed(
                &state.current_entry.snapshot.recipe,
                std::slice::from_ref(&drawn),
            );
            // A layer that draws the mask, so every path below has to resolve its strokes.
            painted.layers.push(crate::Layer {
                mask: Some(painted.masks[0].id.clone()),
                ..crate::Layer::new(crate::BASIC_EFFECT, json!({"exposure": 0.5}))
            });
            let entry = next_entry(&state, painted);
            drop(service);
            commit(&catalog, &entry);

            let before = stored_entry_json(&catalog, &entry.id);
            let connection = Connection::open(&catalog).unwrap();
            if what {
                // Tamper with the stored bytes under an address that still names the old ones.
                connection
                    .execute(
                        "DELETE FROM strokes WHERE id=?1",
                        params![drawn.id().as_str()],
                    )
                    .unwrap();
                connection
                    .execute(
                        "INSERT INTO strokes (id,stroke_json) VALUES (?1,?2)",
                        params![
                            drawn.id().as_str(),
                            r#"{"points":[[1,1]],"size":819,"feather":0.0,"flow":100.0,"erase":false}"#
                        ],
                    )
                    .unwrap();
            } else {
                connection
                    .execute(
                        "DELETE FROM strokes WHERE id=?1",
                        params![drawn.id().as_str()],
                    )
                    .unwrap();
            }
            drop(connection);

            let service = EditorService::open(&catalog).unwrap();
            // Reading, listing and lineage keep working: the stack is retained whole.
            let read = service.entry(&asset, &entry.id).unwrap();
            assert_eq!(
                read.snapshot.recipe.masks, entry.snapshot.recipe.masks,
                "the stored mask table is retained unchanged"
            );
            assert!(service.history(&asset, None, 10).is_ok());
            assert!(service.describe_entry(&asset, None).is_ok());
            assert!(service.state(&asset).is_ok());
            drop(service);
            assert_eq!(
                stored_entry_json(&catalog, &entry.id),
                before,
                "nothing was rewritten or discarded"
            );

            // And every path that would have to draw it refuses by name. `render` and `sample` are
            // the delivered evaluation paths and an image export is not implemented yet; all three
            // compile the recipe through one function, which compiles the bound mask and resolves
            // its strokes there, so the refusal is asserted on the compile every one of them makes.
            // Admission refuses to write it with the same words.
            let registry = ModuleRegistry::builtin();
            let source = crate::SourceImage {
                width: 8,
                height: 8,
                rgba: vec![255; 8 * 8 * 4].into(),
                fingerprint: "test".into(),
                orientation: 1,
                capture: Default::default(),
            };
            let recipe = &read.snapshot.recipe;
            let expected = format!(
                "stroke {} of entry {} {} referenced by component Brush 1 of mask Mask 1",
                drawn.id(),
                entry.id,
                if what {
                    "does not match its stored content address"
                } else {
                    "is not in the stroke store"
                },
            );
            for error in [
                crate::render::testing::render(&registry, &source, SnapshotId::new(), recipe)
                    .unwrap_err(),
                crate::render::testing::sample(&registry, &source, recipe, 0, 0).unwrap_err(),
                crate::render::testing::extents(&registry, &source, recipe).unwrap_err(),
                crate::stage_transform(&registry, source.width, source.height, recipe).unwrap_err(),
                registry
                    .compile(source.width, source.height, recipe)
                    .err()
                    .expect("compiling refuses a broken reference"),
                registry.validate_recipe(recipe).unwrap_err(),
            ] {
                assert_eq!(error.kind, ErrorKind::Incompatible);
                assert_eq!(error.detail, expected);
            }
            std::fs::remove_file(catalog).unwrap();
        }
    }

    /// A commit writes only the strokes its command captured fresh, not the whole mask table's
    /// references: painting a second stroke onto a mask a first stroke already committed issues one
    /// insert, not two, because the current recipe's stroke table was read back from the catalog
    /// before the second command planned against it, and a fresh hydration knows every stroke it
    /// resolved is already durable.
    #[test]
    fn a_commit_writes_only_the_strokes_it_captured_fresh() {
        let catalog = temp("fresh-stroke-writes.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;

        let stroke_request = |points: Value| {
            json!({
                "points": points,
                "size": 0.1,
                "feather": 50.0,
                "flow": 100.0,
                "erase": false,
            })
        };

        crate::editor::stroke_writes::take();
        let revision = service.revision(&asset).unwrap();
        service
            .run_action(
                &asset,
                mutation(revision, "paint-1"),
                commands::ADD_STROKE,
                MaskTarget::default().request(stroke_request(json!([[0.2, 0.2], [0.4, 0.4]]))),
            )
            .unwrap();
        assert_eq!(
            crate::editor::stroke_writes::take(),
            1,
            "the first stroke of a new mask: one row written"
        );

        let recipe = service.state(&asset).unwrap().current_entry.snapshot.recipe;
        let target = MaskTarget {
            mask: Some(recipe.masks[0].id.clone()),
            component: Some(recipe.masks[0].components[0].id.clone()),
            ..MaskTarget::default()
        };
        let revision = service.revision(&asset).unwrap();
        service
            .run_action(
                &asset,
                mutation(revision, "paint-2"),
                commands::ADD_STROKE,
                target.request(stroke_request(json!([[0.6, 0.6], [0.7, 0.5]]))),
            )
            .unwrap();
        assert_eq!(
            crate::editor::stroke_writes::take(),
            1,
            "the second stroke: one row written, and not the first stroke again"
        );

        // Undo, then redo: neither writes an entry, so neither issues a stroke row either.
        let revision = service.revision(&asset).unwrap();
        service.undo(&asset, mutation(revision, "undo")).unwrap();
        let revision = service.revision(&asset).unwrap();
        service.redo(&asset, mutation(revision, "redo")).unwrap();
        assert_eq!(
            crate::editor::stroke_writes::take(),
            0,
            "undo and redo navigate the existing history and write no entry"
        );

        drop(service);
        assert_eq!(
            stored_strokes(&catalog),
            2,
            "both strokes ended up in the store, whichever commit wrote each one"
        );
        std::fs::remove_file(catalog).unwrap();
    }

    /// A second consumer's strokes commit through the same content-addressed store as the brush's,
    /// by the same fresh-only rule: a write stores each address it does not already know is stored,
    /// once, as the canonical bytes of whichever type declared it; a read hands each back as its own
    /// type, known stored, so writing the read table again writes nothing, and only what is added
    /// after it is written. The brush stroke's stored row is its canonical bytes under its pinned
    /// address, exactly as before the store was shared.
    #[test]
    fn a_second_consumers_strokes_commit_through_the_same_store_fresh_only() {
        use crate::path::{StrokeKind, StrokeTable, tests::RepairStroke};
        let catalog = temp("second-consumer-strokes.sqlite");
        drop(EditorService::open(&catalog).unwrap());
        let brush = crate::mask::Stroke::capture(
            &[[0.1, 0.2], [0.4, 0.45], [0.8, 0.2]],
            0.05,
            37.5,
            80.0,
            true,
        )
        .unwrap();
        let first = RepairStroke::capture(&[[0.2, 0.2], [0.5, 0.3]], 0.05, [0.1, 0.0]).unwrap();
        let second = RepairStroke::capture(&[[0.6, 0.6], [0.7, 0.8]], 0.02, [0.0, -0.1]).unwrap();
        let mut table = StrokeTable::new("the two consumers");
        let brush_id = table.insert(brush.clone());
        table.insert(first.clone());
        table.insert(second.clone());
        let mut references = vec![
            crate::path::StrokeReference {
                what: "component Brush 1 of mask Mask 1".to_owned(),
                id: brush_id.clone(),
                kind: crate::path::StrokeType::of::<crate::mask::Stroke>(),
            },
            first.reference("repair 1"),
            second.reference("repair 2"),
            // One address twice is one row.
            first.reference("repair 3"),
        ];
        let write = |references: &[crate::path::StrokeReference], table: &StrokeTable| {
            let mut connection = Connection::open(&catalog).unwrap();
            let tx = connection.transaction().unwrap();
            super::write_fresh_strokes(&tx, references, table).unwrap();
            tx.commit().unwrap();
            crate::editor::stroke_writes::take()
        };
        crate::editor::stroke_writes::take();
        assert_eq!(
            write(&references, &table),
            3,
            "every fresh address once, whichever consumer declared it"
        );
        assert_eq!(stored_strokes(&catalog), 3);

        let connection = Connection::open(&catalog).unwrap();
        let read = super::read_strokes(&connection, references.clone(), "the read back").unwrap();
        assert_eq!(read.get::<crate::mask::Stroke>(&brush_id), Some(&brush));
        assert_eq!(read.get::<RepairStroke>(&first.id()), Some(&first));
        assert_eq!(read.get::<RepairStroke>(&second.id()), Some(&second));
        let stored: String = connection
            .query_row(
                "SELECT stroke_json FROM strokes WHERE id=?1",
                params![brush_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(brush_id.as_str(), "f90567c23419da29e9a57267732c52b1");
        assert_eq!(
            stored,
            r#"{"points":[[1638,3277],[6554,7373],[13107,3277]],"size":819,"feather":38,"flow":80,"erase":true}"#,
            "the brush's stored row is the bytes it always was"
        );
        drop(connection);

        assert_eq!(
            write(&references, &read),
            0,
            "a table read out of the store writes nothing again"
        );
        let mut next = read.clone();
        let third = RepairStroke::capture(&[[0.3, 0.9]], 0.3, [0.0, 0.0]).unwrap();
        next.insert(third.clone());
        references.push(third.reference("repair 4"));
        assert_eq!(
            write(&references, &next),
            1,
            "only the stroke captured after the read"
        );
        assert_eq!(stored_strokes(&catalog), 4);
        std::fs::remove_file(catalog).unwrap();
    }

    /// Every way a stroke can enter a recipe, in one session: painting, undo onto an older entry
    /// then painting again — a new branch — a copied recipe (`mask.duplicate`, which references
    /// existing strokes under new component identities and stores nothing new), and a draft's
    /// commit carrying stroke data through the same `mask.add-stroke` request a hand-drawn stroke
    /// takes.
    ///
    /// `store_strokes` now writes only what a table does not already know is stored, rather than
    /// every reference the recipe carries; this proves that shortcut never skips a stroke that was
    /// only ever *referenced*, not stored, by reopening the catalog afterwards and resolving every
    /// entry's every reference, whichever path put it there.
    #[test]
    fn a_reopened_catalog_resolves_every_stroke_across_every_way_one_enters_a_recipe() {
        let catalog = temp("every-stroke-entry-path.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;

        let stroke_request = |points: Value| {
            json!({
                "points": points,
                "size": 0.1,
                "feather": 50.0,
                "flow": 100.0,
                "erase": false,
            })
        };

        // Painting: the first stroke starts a mask, the second lands in the same component.
        let revision = service.revision(&asset).unwrap();
        service
            .run_action(
                &asset,
                mutation(revision, "paint-a"),
                commands::ADD_STROKE,
                MaskTarget::default().request(stroke_request(json!([[0.1, 0.1], [0.2, 0.2]]))),
            )
            .unwrap();
        let after_a = service.state(&asset).unwrap();
        let brush = MaskTarget {
            mask: Some(after_a.current_entry.snapshot.recipe.masks[0].id.clone()),
            component: Some(
                after_a.current_entry.snapshot.recipe.masks[0].components[0]
                    .id
                    .clone(),
            ),
            ..MaskTarget::default()
        };
        let revision = service.revision(&asset).unwrap();
        service
            .run_action(
                &asset,
                mutation(revision, "paint-b"),
                commands::ADD_STROKE,
                brush
                    .clone()
                    .request(stroke_request(json!([[0.3, 0.3], [0.4, 0.4]]))),
            )
            .unwrap();

        // Undo back onto the first stroke's own entry, then paint a third: a new branch off an
        // entry this session already committed and cached, not the latest one.
        let revision = service.revision(&asset).unwrap();
        service.undo(&asset, mutation(revision, "undo")).unwrap();
        assert_eq!(
            service.state(&asset).unwrap().current_entry.id,
            after_a.current_entry.id,
            "undo landed back on the first stroke's entry"
        );
        let revision = service.revision(&asset).unwrap();
        service
            .run_action(
                &asset,
                mutation(revision, "paint-c-branch"),
                commands::ADD_STROKE,
                brush.request(stroke_request(json!([[0.5, 0.1], [0.6, 0.2]]))),
            )
            .unwrap();

        // A copied recipe: `mask.duplicate` references the branch's existing strokes under new
        // component identities rather than storing anything new.
        let branched = service.state(&asset).unwrap();
        let source_mask = branched.current_entry.snapshot.recipe.masks[0].id.clone();
        let revision = service.revision(&asset).unwrap();
        service
            .run_action(
                &asset,
                mutation(revision, "duplicate"),
                "mask.duplicate",
                MaskTarget {
                    mask: Some(source_mask),
                    ..MaskTarget::default()
                }
                .request(json!({})),
            )
            .unwrap();

        // A draft's commit: the same `mask.add-stroke` request an agent's `draft.set` would carry,
        // targeting the duplicate's own component.
        let duplicated = service.state(&asset).unwrap();
        let copy = &duplicated.current_entry.snapshot.recipe.masks[1];
        let mut draft = Draft::new(commands::ADD_STROKE, asset.clone(), duplicated.revision);
        draft.target = MaskTarget {
            mask: Some(copy.id.clone()),
            component: Some(copy.components[0].id.clone()),
            ..MaskTarget::default()
        }
        .identities();
        draft.merge(
            stroke_request(json!([[0.7, 0.7], [0.8, 0.6]]))
                .as_object()
                .unwrap()
                .clone(),
        );
        let revision = service.revision(&asset).unwrap();
        service
            .run_action(
                &asset,
                mutation(revision, "draft-commit"),
                &draft.action,
                Value::Object(draft.request()),
            )
            .unwrap();
        drop(service);

        // Reopen fresh and walk the whole history: every entry's referenced strokes resolve, none
        // faulted, whatever path put them in the recipe.
        let service = EditorService::open(&catalog).unwrap();
        let page = service.history(&asset, None, 50).unwrap();
        assert_eq!(
            page.entries.len(),
            6,
            "the Original plus the five commits above, including the undone branch's own entry"
        );
        for row in &page.entries {
            let entry = service.entry(&asset, &row.id).unwrap();
            assert!(
                !entry.snapshot.recipe.strokes.has_missing(),
                "entry {} ({}) references a stroke the store cannot resolve",
                row.sequence,
                row.label,
            );
            for reference in entry.snapshot.recipe.stroke_references().unwrap() {
                let id = &reference.id;
                if let Err(error) = entry.snapshot.recipe.strokes.check_reference(&reference) {
                    panic!(
                        "entry {} ({}) stroke {id}: {error}",
                        row.sequence, row.label
                    );
                }
            }
        }
        drop(service);
        assert_eq!(
            stored_strokes(&catalog),
            4,
            "four distinct strokes: the duplicate and the draft referenced rather than storing again"
        );
        std::fs::remove_file(catalog).unwrap();
    }

    /// Strokes per mask in the measured session below: the smaller of what the points-per-mask
    /// limit admits at 100 positions a stroke (81) and the brush component's own declared 64 strokes
    /// per component. It is why the 200-stroke session paints four masks — 200 strokes of 100
    /// positions cannot live in one mask at all — and the two assertions below are what keep the
    /// three limits from drifting apart.
    const STROKES_PER_MASK: usize = crate::mask::STROKES_PER_COMPONENT;
    const _: () = assert!(STROKES_PER_MASK * 100 <= crate::POINTS_PER_MASK);
    const _: () = assert!(STROKES_PER_MASK <= crate::mask::STROKES_PER_COMPONENT);

    /// A mask holding these strokes by address, with a component named for its ordinal so several
    /// masks in one recipe read apart.
    fn brush_mask(
        name: &str,
        table: &mut crate::path::StrokeTable,
        strokes: &[crate::mask::Stroke],
    ) -> Mask {
        let addresses: Vec<String> = strokes
            .iter()
            .map(|stroke| table.insert(stroke.clone()).to_string())
            .collect();
        let mut mask = Mask::new(name);
        let component = mask.next_component_name("brush");
        mask.components.push(Component::new(
            component,
            ComponentMode::Add,
            "brush",
            json!({ "strokes": addresses }),
        ));
        mask
    }

    /// What one session of `count` strokes costs across its history, content-addressed against
    /// embedded, measured on the bytes that are actually written.
    ///
    /// The store does not change the *shape* of the growth: an entry still holds one reference per
    /// stroke, so the total is still quadratic in the stroke count. What it changes is the constant
    /// — a reference instead of a stroke — and that is the only claim measured here.
    fn session_bytes(catalog: &Path, count: usize) -> (usize, usize, usize) {
        let mut service = EditorService::open(catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let base = state.current_entry.snapshot.recipe.clone();
        drop(service);

        let registry = ModuleRegistry::builtin();
        let mut connection = Connection::open(catalog).unwrap();
        let tx = connection.transaction().unwrap();
        let mut table = crate::path::StrokeTable::new("the measured session");
        let mut drawn: Vec<Vec<crate::mask::Stroke>> = Vec::new();
        let mut previous = state.current_entry.clone();
        let mut revision = state.revision;
        // What an entry embedding its strokes would have cost, accumulated beside what the
        // content-addressed entries actually cost.
        let mut embedded = 0_usize;
        for index in 0..count {
            let one = stroke(index);
            if index.is_multiple_of(STROKES_PER_MASK) {
                drawn.push(Vec::new());
            }
            drawn.last_mut().unwrap().push(one);
            let masks: Vec<Mask> = drawn
                .iter()
                .enumerate()
                .map(|(at, strokes)| brush_mask(&format!("Mask {}", at + 1), &mut table, strokes))
                .collect();
            let recipe = Recipe {
                masks,
                strokes: table.clone(),
                ..base.clone()
            };
            // The same snapshot with every stroke's positions written into the payload instead of
            // its address: the shape this store exists to avoid.
            embedded += drawn
                .iter()
                .flatten()
                .map(|stroke| stroke.canonical().len() + 1)
                .sum::<usize>();
            let entry = HistoryEntry {
                id: EntryId::new(),
                sequence: previous.sequence + 1,
                label: format!("Brush {}", index + 1),
                undo_parent: Some(previous.id.clone()),
                base_revision: revision,
                result_revision: revision + 1,
                snapshot: Snapshot {
                    id: SnapshotId::new(),
                    asset_id: asset.clone(),
                    recipe,
                },
                ..previous.clone()
            };
            revision += 1;
            registry.validate_recipe(&entry.snapshot.recipe).unwrap();
            insert_entry(&tx, &default_artifact_root(catalog), &entry).unwrap();
            previous = entry;
        }
        tx.commit().unwrap();

        let entries: i64 = connection
            .query_row("SELECT SUM(LENGTH(entry_json)) FROM entries", [], |row| {
                row.get(0)
            })
            .unwrap();
        let strokes: i64 = connection
            .query_row("SELECT SUM(LENGTH(stroke_json)) FROM strokes", [], |row| {
                row.get(0)
            })
            .unwrap();
        let addressed = entries as usize + strokes as usize;
        (
            addressed,
            addressed - strokes as usize + embedded,
            strokes as usize,
        )
    }

    /// The measurement the content-addressed store is justified by: what 200 strokes of 100
    /// positions cost across a history of 200 entries.
    ///
    /// Scope: `luxforge-core`'s own catalog, on the JPEG fixture, counting the bytes of every
    /// stored entry plus the bytes of the stroke store, against the same entries with each stroke's
    /// positions embedded in its payload. It is a byte count and not a timing, so no load average
    /// applies to it. The strokes are spread over four masks because the declared points-per-mask
    /// limit admits at most 81 strokes of 100 positions in one mask.
    #[test]
    fn two_hundred_strokes_cost_about_a_megabyte_rather_than_about_forty() {
        let catalog = temp("stroke-growth.sqlite");
        let (addressed, embedded, distinct) = session_bytes(&catalog, 200);
        std::fs::remove_file(&catalog).unwrap();
        let mb = |bytes: usize| bytes as f64 / 1_000_000.0;
        println!(
            "200 strokes of 100 positions: distinct stroke data {} KiB, content-addressed across \
             history {:.2} MB, embedded across history {:.2} MB, a factor of {:.1}; one stroke \
             serializes to {} bytes and one reference costs 35",
            distinct / 1024,
            mb(addressed),
            mb(embedded),
            embedded as f64 / addressed as f64,
            distinct / 200,
        );
        // The design's table carries these measured figures, and carried predicted ones before this
        // ran: it predicted 364 KiB of distinct stroke data and 37.5 MB embedded, from a stroke
        // serializing to about 1.8 KiB. A stored position is a whole grid step and not a decimal, so
        // a stroke serializes to about 968 bytes, and the table was corrected to what is measured
        // here rather than the prediction being kept. The content-addressed total was predicted at
        // 1.08 MB and measures 1.10 MB, because it is dominated by the 35-byte reference, which is
        // what the prediction got right.
        assert!(
            (0.95..1.25).contains(&mb(addressed)),
            "content-addressed history measured {:.3} MB, not the recorded 1.11 MB",
            mb(addressed),
        );
        assert!(
            (18.0..23.0).contains(&mb(embedded)),
            "embedded history measured {:.3} MB, not the recorded 20.4 MB",
            mb(embedded),
        );
        assert!(
            (170..210).contains(&(distinct / 1024)),
            "distinct stroke data measured {} KiB, not the recorded 189 KiB",
            distinct / 1024,
        );
        // The store shrinks the constant; it does not change the shape of the growth, which is
        // still quadratic in the stroke count. This is the constant, per stroke per entry.
        assert!(
            (25..32).contains(&(distinct / 200 / 35)),
            "one reference stands in for {} of its own size, not the recorded 28",
            distinct / 200 / 35,
        );
    }

    /// One sample of a painting session's stored cost, taken after the entry that carried its last
    /// stroke was committed.
    ///
    /// Everything here is a byte count taken from the catalog itself, so nothing in it depends on
    /// what else the host is doing.
    #[derive(Clone, Copy)]
    struct Growth {
        strokes: usize,
        /// The bytes of every stored entry's JSON: the snapshots, which is where the growth is.
        entries: usize,
        /// The bytes of the content-addressed store: each distinct stroke once.
        store: usize,
        /// What the same entries would have cost with each stroke's positions written into its
        /// payload instead of its address: the shape the store exists to avoid.
        embedded: usize,
    }

    impl Growth {
        /// Entries plus store: what a painting session costs a catalog, and the number the curve is
        /// read from.
        fn stored(self) -> usize {
            self.entries + self.store
        }
    }

    /// Pack a session's strokes into the densest mask table the declared limits admit, or `None`
    /// for a session too large to be a recipe at all.
    ///
    /// A component takes [`crate::mask::STROKES_PER_COMPONENT`] strokes and a mask takes components
    /// until its strokes' stored positions would pass [`crate::POINTS_PER_MASK`], so the point bound
    /// closes a mask long before its 32 components do for any stroke of usable length.
    /// [`crate::MASKS_PER_RECIPE`] masks of those is the ceiling, and a session past it is not a
    /// recipe this build will hold; that ceiling is the real end of the quadratic curve and is
    /// measured rather than assumed.
    fn packed(addresses: &[String], lengths: &[usize]) -> Option<Vec<Mask>> {
        fn close(mask: &mut Mask, component: &mut Vec<String>) {
            if component.is_empty() {
                return;
            }
            let name = mask.next_component_name("brush");
            mask.components.push(Component::new(
                name,
                ComponentMode::Add,
                "brush",
                json!({ "strokes": std::mem::take(component) }),
            ));
        }
        let mut masks: Vec<Mask> = Vec::new();
        let mut mask = Mask::new("Mask 1");
        let mut component: Vec<String> = Vec::new();
        let mut points = 0_usize;
        for (address, length) in addresses.iter().zip(lengths) {
            if points + length > crate::POINTS_PER_MASK
                || (component.len() == crate::mask::STROKES_PER_COMPONENT
                    && mask.components.len() == crate::COMPONENTS_PER_MASK)
            {
                close(&mut mask, &mut component);
                let next = Mask::new(format!("Mask {}", masks.len() + 2));
                masks.push(std::mem::replace(&mut mask, next));
                points = 0;
            }
            if component.len() == crate::mask::STROKES_PER_COMPONENT {
                close(&mut mask, &mut component);
            }
            component.push(address.clone());
            points += length;
        }
        close(&mut mask, &mut component);
        masks.push(mask);
        (masks.len() <= crate::MASKS_PER_RECIPE).then_some(masks)
    }

    /// Paint `count` strokes into `catalog` over `source`, one stroke per history entry written
    /// through the production write path, sampling the stored cost every `every` strokes.
    ///
    /// The strokes are packed by [`packed`], so the session is the densest one the declared limits
    /// admit and its cost is the worst case rather than an arrangement chosen to be cheap.
    ///
    /// `table` is marked [`crate::path::StrokeTable::mark_stored`] for the one stroke each commit
    /// captured, right after that commit lands, which is what a fresh hydration of the entry this
    /// harness just wrote would mark on its own: this session builds its recipes directly rather
    /// than through `mask.add-stroke` and a re-read, so it has to say so itself, for the same reason
    /// `insert_entry` is still the real one — the point is the production write path, not a harness
    /// that happens to look like it.
    ///
    /// The returned asset is the painted one, so a caller can reopen the catalog it left.
    fn painting_session(
        catalog: &Path,
        source: &Path,
        count: usize,
        every: usize,
    ) -> (Vec<Growth>, AssetId) {
        let mut service = EditorService::open(catalog).unwrap();
        let asset = service.import(source).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let base = state.current_entry.snapshot.recipe.clone();
        drop(service);

        let registry = ModuleRegistry::builtin();
        let mut connection = Connection::open(catalog).unwrap();
        let mut table = crate::path::StrokeTable::new("the measured session");
        // One address per stroke, kept rather than recomputed, so building the recipe an entry
        // carries costs a clone of the table and not a rehash of every stroke drawn so far.
        let mut addresses: Vec<String> = Vec::with_capacity(count);
        let mut previous = state.current_entry.clone();
        let mut revision = state.revision;
        // The counterfactual, accumulated beside the real thing: `drawn` is what this session's
        // strokes serialize to in full, and every entry embeds all of them, so `embedded` grows by
        // the whole of `drawn` once per entry. That is the quadratic term with its large constant.
        // The entries' own bytes are added to it at each checkpoint, exactly as the 200-stroke
        // measurement does, so the two tables are read against each other directly.
        let mut drawn = 0_usize;
        let mut embedded = 0_usize;
        let mut lengths: Vec<usize> = Vec::with_capacity(count);
        let mut curve = Vec::new();
        for index in 0..count {
            let one = stroke(index);
            drawn += one.canonical().len() + 1;
            embedded += drawn;
            lengths.push(one.point_count());
            let id = table.insert(one);
            addresses.push(id.to_string());
            let masks = packed(&addresses, &lengths)
                .expect("this session is past the per-recipe mask ceiling and is not a recipe");
            let entry = HistoryEntry {
                id: EntryId::new(),
                sequence: previous.sequence + 1,
                label: format!("Brush {}", index + 1),
                undo_parent: Some(previous.id.clone()),
                base_revision: revision,
                result_revision: revision + 1,
                snapshot: Snapshot {
                    id: SnapshotId::new(),
                    asset_id: asset.clone(),
                    recipe: Recipe {
                        masks,
                        strokes: table.clone(),
                        ..base.clone()
                    },
                },
                ..previous.clone()
            };
            revision += 1;
            registry.validate_recipe(&entry.snapshot.recipe).unwrap();
            let tx = connection.transaction().unwrap();
            insert_entry(&tx, &default_artifact_root(catalog), &entry).unwrap();
            tx.execute(
                "UPDATE asset_state SET current_entry_id=?1, revision=?2 WHERE asset_id=?3",
                params![entry.id.as_str(), revision as i64, asset.as_str()],
            )
            .unwrap();
            tx.commit().unwrap();
            // This stroke is durable now: the next commit's fresh hydration would mark it stored on
            // its own, and this harness has to say so itself because it never re-reads the entry it
            // just wrote.
            table.mark_stored(id);
            previous = entry;
            if (index + 1).is_multiple_of(every) || index + 1 == count {
                let entries: i64 = connection
                    .query_row("SELECT SUM(LENGTH(entry_json)) FROM entries", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                let store: i64 = connection
                    .query_row("SELECT SUM(LENGTH(stroke_json)) FROM strokes", [], |row| {
                        row.get(0)
                    })
                    .unwrap();
                curve.push(Growth {
                    strokes: index + 1,
                    entries: entries as usize,
                    store: store as usize,
                    embedded: entries as usize + embedded,
                });
            }
        }
        drop(connection);
        (curve, asset)
    }

    /// Least squares over `S(n) = a·n² + b·n + c`, returned as `(a, b, c)`.
    ///
    /// The point of fitting rather than asserting a ratio is that the quadratic term is then a
    /// number on the page: a session's cost is not linear in its stroke count and this is what says
    /// so.
    fn quadratic_fit(curve: &[Growth]) -> (f64, f64, f64) {
        // Normal equations for the three-column design matrix [n², n, 1]. Six moments of n and three
        // of S are all it needs, and the 3×3 solve is written out rather than looped.
        let (mut s0, mut s1, mut s2, mut s3, mut s4) = (0.0, 0.0, 0.0, 0.0, 0.0);
        let (mut t0, mut t1, mut t2) = (0.0, 0.0, 0.0);
        for point in curve {
            let n = point.strokes as f64;
            let y = point.stored() as f64;
            s0 += 1.0;
            s1 += n;
            s2 += n * n;
            s3 += n * n * n;
            s4 += n * n * n * n;
            t0 += y;
            t1 += n * y;
            t2 += n * n * y;
        }
        let m = [[s4, s3, s2], [s3, s2, s1], [s2, s1, s0]];
        let rhs = [t2, t1, t0];
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        let solve = |column: usize| {
            let mut c = m;
            for (row, value) in rhs.iter().enumerate() {
                c[row][column] = *value;
            }
            (c[0][0] * (c[1][1] * c[2][2] - c[1][2] * c[2][1])
                - c[0][1] * (c[1][0] * c[2][2] - c[1][2] * c[2][0])
                + c[0][2] * (c[1][0] * c[2][1] - c[1][1] * c[2][0]))
                / det
        };
        (solve(0), solve(1), solve(2))
    }

    /// The largest session of these strokes a recipe can hold: [`crate::MASKS_PER_RECIPE`] masks,
    /// each filled to [`crate::POINTS_PER_MASK`] stored positions. It is where the quadratic curve
    /// stops, which is what makes its far end a bounded number rather than an extrapolation, and it
    /// is measured by the test below rather than reasoned out — these strokes are captured at 100
    /// positions and decimate to between 67 and 78, so the arithmetic on 100 would be wrong.
    const CEILING: usize = 1809;

    /// The growth is quadratic, and its square term is the one the design records.
    ///
    /// Scope: `luxforge-core`'s own catalog on the 24 MP generated fixture when it has been
    /// generated and on the small JPEG fixture otherwise — the catalog's bytes do not depend on the
    /// source's pixel dimensions, as the recorded 24 and 60 MP sessions agree — one stroke of 100
    /// positions per history entry, counting the bytes of every stored entry plus the bytes of the
    /// stroke store. Byte counts, so no load average applies.
    ///
    /// It gates the shape at 400 strokes; the thousand-stroke and ceiling figures in
    /// `docs/design/masking.md` are recorded measurements, so the suite does not carry a
    /// twenty-second session to learn what four hundred strokes already say.
    #[test]
    fn a_painting_session_grows_with_the_square_of_its_stroke_count() {
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated/24mp.jpg");
        let source = if source.exists() { source } else { fixture() };
        let catalog = temp("quadratic-growth.sqlite");
        let (curve, _) = painting_session(&catalog, &source, 400, 100);
        std::fs::remove_file(&catalog).unwrap();
        let at = |n: usize| {
            curve
                .iter()
                .find(|point| point.strokes == n)
                .copied()
                .expect("a sampled stroke count")
        };
        let (quadratic, linear, _) = quadratic_fit(&curve);
        println!(
            "400 strokes: {:.3} MB stored, {:.1} MB embedded; S(n) = {quadratic:.2}·n² + {linear:.0}·n",
            at(400).stored() as f64 / 1e6,
            at(400).embedded as f64 / 1e6,
        );
        // Doubling the stroke count multiplies the stored bytes by about four. That is the whole
        // claim about the shape, and it is what forbids anyone writing that the store made the
        // growth linear. The ratio is a little under four because the linear term has not washed
        // out at these counts; it approaches four as the session grows.
        let doubling = at(400).stored() as f64 / at(200).stored() as f64;
        assert!(
            (3.2..4.3).contains(&doubling),
            "doubling the strokes multiplied the bytes by {doubling:.2}; quadratic growth doubles \
             to about four and linear growth to two",
        );
        // The fitted square term is the design's recorded curve: about twenty bytes per stroke per
        // stroke, which is the 35-byte reference paid by half the entries on average, plus the mask
        // and component structure it hangs on.
        assert!(
            (17.0..23.0).contains(&quadratic),
            "the fitted n² coefficient is {quadratic:.2} bytes, not the recorded 19.6",
        );
    }

    /// A recipe of 100-position strokes has a ceiling, and it is the masks-per-recipe limit rather
    /// than anything about the store.
    ///
    /// This is what makes the quadratic curve's far end a bounded number: the design's figure for
    /// 2400 strokes describes a session no recipe of strokes this length can hold, because the
    /// per-mask point bound admits about 113 of them and a recipe holds sixteen masks.
    #[test]
    fn a_session_of_long_strokes_ends_at_the_masks_per_recipe_ceiling() {
        let lengths: Vec<usize> = (0..CEILING * 2)
            .map(|index| stroke(index).point_count())
            .collect();
        let addresses: Vec<String> = (0..CEILING * 2).map(|i| format!("{i:032x}")).collect();
        let ceiling = (1..lengths.len())
            .take_while(|count| packed(&addresses[..*count], &lengths[..*count]).is_some())
            .last()
            .expect("at least one stroke packs");
        println!(
            "the measured session's strokes hold {}..={} positions each and {ceiling} of them is \
             the most a recipe can hold",
            lengths.iter().min().unwrap(),
            lengths.iter().max().unwrap(),
        );
        // The recorded curve ends at CEILING, so CEILING has to be a session that packs, and the
        // stroke past it has to be one that does not: that is what makes the curve's far end the
        // real end rather than a number chosen to be round.
        assert_eq!(ceiling, CEILING);
        let full = packed(&addresses[..ceiling], &lengths[..ceiling]).expect("the ceiling packs");
        assert_eq!(full.len(), crate::MASKS_PER_RECIPE);
        for mask in &full {
            let points: usize = mask
                .components
                .iter()
                .flat_map(|component| {
                    crate::path::references(&component.payload, "a packed component").unwrap()
                })
                .map(|id| {
                    let at = usize::from_str_radix(id.as_str(), 16).expect("a positional address");
                    lengths[at]
                })
                .sum();
            assert!(points <= crate::POINTS_PER_MASK);
            assert!(mask.components.len() <= crate::COMPONENTS_PER_MASK);
        }
    }

    /// One single-position stroke, distinct per index, for the sessions that press a count rather
    /// than a length.
    fn tiny_stroke(index: usize) -> crate::mask::Stroke {
        let x = 0.1 + (index % 4096) as f64 / 16384.0;
        let y = 0.1 + (index / 4096) as f64 / 16384.0;
        crate::mask::Stroke::capture(&[[x, y]], 0.04, 50.0, 100.0, false).expect("a legal stroke")
    }

    /// The per-recipe serialized mask bound is the one that keeps a painting session's snapshots
    /// bounded, so it is refused by name and the refusal writes nothing.
    ///
    /// The bound is reached with single-position strokes because that is the shape that presses it:
    /// the per-mask point bound stops a session of long strokes long before its references fill
    /// 256 KiB.
    #[test]
    fn a_recipe_over_the_serialized_mask_bound_names_it_and_leaves_the_catalog_as_it_was() {
        let catalog = temp("serialized-mask-bound.sqlite");
        let (_, asset) = painting_session(&catalog, &fixture(), 4, 4);

        // The durable state the refusal must not touch: the catalog's own bytes, and what a reopen
        // reads back out of them.
        let before = std::fs::read(&catalog).unwrap();
        let digest = |bytes: &[u8]| format!("{:x}", Sha256::digest(bytes));
        let service = EditorService::open(&catalog).unwrap();
        let state = service.state(&asset).unwrap();
        let entries = service.history(&asset, None, 64).unwrap().entries.len();
        let base = state.current_entry.snapshot.recipe.clone();
        let current = state.current_entry.id.clone();
        drop(service);

        // Fill masks to their declared component and stroke counts until the mask table serializes
        // past the bound. Nothing else is at its limit: each mask holds 2048 stored positions
        // against a bound of 8192, and the recipe holds four masks against a bound of sixteen.
        let mut table = crate::path::StrokeTable::new("the over-bound session");
        let mut masks: Vec<Mask> = vec![Mask::new("Full 1")];
        let mut drawn = 0_usize;
        let mut under = 0_usize;
        while serde_json::to_vec(&masks).unwrap().len() <= crate::MASK_BYTES_PER_RECIPE {
            under = drawn;
            if masks.last().unwrap().components.len() == crate::COMPONENTS_PER_MASK {
                masks.push(Mask::new(format!("Full {}", masks.len() + 1)));
                assert!(
                    masks.len() <= crate::MASKS_PER_RECIPE,
                    "the mask-count bound was reached before the byte bound, so this test is \
                     pressing the wrong limit"
                );
            }
            let addresses: Vec<String> = (0..crate::mask::STROKES_PER_COMPONENT)
                .map(|_| {
                    drawn += 1;
                    table.insert(tiny_stroke(drawn)).to_string()
                })
                .collect();
            let mask = masks.last_mut().unwrap();
            let name = mask.next_component_name("brush");
            mask.components.push(Component::new(
                name,
                ComponentMode::Add,
                "brush",
                json!({ "strokes": addresses }),
            ));
        }
        let bytes = serde_json::to_vec(&masks).unwrap().len();
        // The byte bound is also the absolute ceiling on a recipe's stroke count, because a
        // reference costs 35 bytes whatever it points at: no recipe of any shape holds more strokes
        // than this, whatever its strokes are, and no snapshot a painting session writes is larger
        // than the bound.
        println!(
            "{under} single-position strokes over {} masks are the most that fit the {} KiB bound; \
             {drawn} of them serialize to {bytes} bytes and are refused",
            masks.len(),
            crate::MASK_BYTES_PER_RECIPE / 1024,
        );
        let over = Snapshot {
            id: SnapshotId::new(),
            asset_id: asset.clone(),
            recipe: Recipe {
                masks,
                strokes: table,
                ..base
            },
        };

        // Committed through the one write path, which admits the stack before it opens a
        // transaction.
        let mut service = EditorService::open(&catalog).unwrap();
        let error = service
            .commit_snapshot(
                &asset,
                mutation(state.revision, "over"),
                json!({"action": "brush"}),
                over,
                &state.asset,
                crate::editor::history::CommittedAction {
                    input: crate::modules::ActionInput {
                        action_id: "brush".into(),
                        parameters: serde_json::Map::new(),
                    },
                    label: "Brush past the bound".into(),
                    touched: None,
                    skipped: Vec::new(),
                },
            )
            .expect_err("past the serialized bound");
        drop(service);
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(
            error.detail,
            format!(
                "recipe masks serialize to {bytes} bytes; the limit is {} serialized mask bytes \
                 per recipe",
                crate::MASK_BYTES_PER_RECIPE
            ),
        );

        // Byte for byte as it was: the bound is checked before anything is written, so the refused
        // stack left neither an entry row, a request nor a stroke in the store.
        let after = std::fs::read(&catalog).unwrap();
        assert_eq!(
            digest(&before),
            digest(&after),
            "the refused write changed the catalog file"
        );
        let service = EditorService::open(&catalog).unwrap();
        let reopened = service.state(&asset).unwrap();
        assert_eq!(reopened.current_entry.id, current);
        assert_eq!(
            service.history(&asset, None, 64).unwrap().entries.len(),
            entries
        );
        drop(service);
        std::fs::remove_file(&catalog).unwrap();
    }

    /// The declared points-per-mask limit is enforced where a recipe enters the service, with the
    /// strokes in hand, and names itself.
    #[test]
    fn a_mask_over_the_points_per_mask_limit_names_the_limit() {
        let catalog = temp("points-per-mask.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let state = service.state(&asset).unwrap();
        let base = state.current_entry.snapshot.recipe.clone();
        drop(service);
        std::fs::remove_file(&catalog).unwrap();

        let registry = ModuleRegistry::builtin();
        let at_bound: Vec<crate::mask::Stroke> = (0..STROKES_PER_MASK).map(stroke).collect();
        let points: usize = at_bound.iter().map(crate::mask::Stroke::point_count).sum();
        let recipe = |strokes: &[crate::mask::Stroke]| {
            let mut table = crate::path::StrokeTable::new("the test session");
            Recipe {
                masks: vec![brush_mask("Mask 1", &mut table, strokes)],
                strokes: table,
                ..base.clone()
            }
        };
        // Under the bound the recipe is admitted: the brush kind is evaluable and the limit has not
        // been reached, so nothing refuses.
        assert!(
            registry.validate_recipe(&recipe(&at_bound)).is_ok(),
            "a mask under the bound should be admitted"
        );
        assert!(points <= crate::POINTS_PER_MASK);

        let mut over = at_bound.clone();
        while over
            .iter()
            .map(crate::mask::Stroke::point_count)
            .sum::<usize>()
            <= crate::POINTS_PER_MASK
        {
            over.push(stroke(over.len()));
        }
        let total: usize = over.iter().map(crate::mask::Stroke::point_count).sum();
        let error = registry
            .validate_recipe(&recipe(&over))
            .expect_err("past the bound");
        assert_eq!(error.kind, ErrorKind::ResourceLimit);
        assert_eq!(
            error.detail,
            format!(
                "mask Mask 1 holds {total} stored path positions; the limit is {} points per mask",
                crate::POINTS_PER_MASK
            )
        );
    }
}
