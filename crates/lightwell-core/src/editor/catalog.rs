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

/// Format 10 stores each asset request's whole answer in the request table — for a `mask.*` command
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
pub(super) const CATALOG_FORMAT: i64 = 10;
pub(super) const ASSET_COLUMNS: &str =
    "id,source_root,locator,fingerprint,file_identity,byte_len,width,height,source_json";

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
    Error::new(ErrorKind::Incompatible, format!("{context}: {error}"))
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

impl EditorService {
    pub(super) fn create_schema(connection: &mut Connection) -> Result<(), Error> {
        write(connection, |tx| {
            let occupied: bool =
                tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_schema)", [], |row| {
                    row.get(0)
                })?;
            if occupied {
                return Err(Error::new(
                    ErrorKind::Incompatible,
                    "unmarked catalog is not empty; choose a new catalog path",
                ));
            }
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
                    source_json TEXT NOT NULL
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
        return Err(Error::new(
            ErrorKind::Conflict,
            "request was committed concurrently",
        ));
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

/// Write this recipe's strokes to the content-addressed store, once each.
///
/// The address is the content's, so a stroke a later entry references again is already there and
/// the insert does nothing: that is the whole of "stored once", and it needs no reference count and
/// no check of what else points at it. The entry's own JSON carries only the addresses, so nothing
/// written here is ever written into an entry.
///
/// A reference the recipe could not resolve writes nothing and is not an error at this boundary: an
/// unresolvable reference is retained data, and the paths that would *draw* it refuse it by name.
fn store_strokes(tx: &Transaction<'_>, recipe: &Recipe) -> Result<(), Error> {
    let references = recipe.stroke_references()?;
    if references.is_empty() {
        return Ok(());
    }
    let mut statement =
        tx.prepare("INSERT OR IGNORE INTO strokes (id,stroke_json) VALUES (?1,?2)")?;
    for (_, id) in &references {
        let Some(stroke) = recipe.strokes.get(id) else {
            continue;
        };
        let text = String::from_utf8(stroke.canonical())
            .map_err(|e| Error::new(ErrorKind::Internal, format!("cannot store stroke: {e}")))?;
        statement.execute(params![id.as_str(), text])?;
    }
    Ok(())
}

/// Resolve this recipe's stroke references against the store, one lookup each.
///
/// Nothing is replayed and no earlier entry is read: an entry is a complete snapshot, and this is
/// the lookup that turns its addresses back into the strokes they name. A reference the store does
/// not hold, or whose stored bytes are not the bytes the address names, is recorded as a fault
/// rather than raised here, so reading, listing, undoing and carrying the stack forward keep
/// working; the refusal happens where the recipe is compiled, which is every path that would draw
/// it.
fn hydrate_strokes(
    connection: &Connection,
    recipe: &mut Recipe,
    origin: &str,
) -> Result<(), Error> {
    let references = recipe.stroke_references()?;
    if references.is_empty() {
        return Ok(());
    }
    let mut table = crate::path::StrokeTable::new(origin);
    let mut statement = connection.prepare("SELECT stroke_json FROM strokes WHERE id=?1")?;
    for (_, id) in references {
        if table.get(&id).is_some() {
            continue;
        }
        let stored: Option<String> = statement
            .query_row(params![id.as_str()], |row| row.get(0))
            .optional()?;
        match stored {
            None => table.fault(id, crate::path::StrokeFault::Missing),
            Some(text) => match crate::path::Stroke::from_stored(&id, text.as_bytes()) {
                Ok(stroke) => {
                    table.insert(stroke);
                }
                Err(_) => table.fault(id, crate::path::StrokeFault::Corrupt),
            },
        }
    }
    recipe.strokes = table;
    Ok(())
}

pub(super) fn next_sequence(connection: &Connection, asset_id: &AssetId) -> Result<u64, Error> {
    let sequence: i64 = connection.query_row(
        "SELECT COALESCE(MAX(sequence),-1)+1 FROM entries WHERE asset_id=?1",
        [asset_id.as_str()],
        |row| row.get(0),
    )?;
    u64::try_from(sequence).map_err(|_| Error::new(ErrorKind::Catalog, "invalid history sequence"))
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
        .ok_or_else(|| Error::new(ErrorKind::Validation, "unknown asset"))?
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
        .ok_or_else(|| Error::new(ErrorKind::Validation, "unknown asset"))
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
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                "history entry does not belong to this asset",
            )
        })?;
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
    use crate::{Component, ComponentMode, Mask, ModuleRegistry, Snapshot, SnapshotId};
    use serde_json::json;
    use std::time::Instant;

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
            assert_eq!(std::fs::read(&catalog).unwrap(), before);
            std::fs::remove_file(catalog).unwrap();
        }
    }

    #[test]
    fn a_format_2_catalog_is_refused_by_name_without_rewriting_it() {
        let catalog = temp("format-2.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        service.import(&fixture()).unwrap();
        drop(service);
        // Entries before format 3 carry no label, so the marker refuses them rather than guessing.
        let connection = Connection::open(&catalog).unwrap();
        connection.pragma_update(None, "user_version", 2).unwrap();
        drop(connection);
        let before = std::fs::read(&catalog).unwrap();
        let error = EditorService::open(&catalog).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(
            error.detail,
            "catalog format 2 is not supported; expected 10; choose a new catalog path"
        );
        assert_eq!(
            std::fs::read(&catalog).unwrap(),
            before,
            "a refused catalog is left byte for byte as it was"
        );
        std::fs::remove_file(catalog).unwrap();
    }

    /// A format 7 catalog's entries hold their label, actor, timestamp and restore target only in
    /// the entry JSON, so a history page could not read its rows from columns: it is refused by name
    /// and left as it is rather than read by decoding every entry.
    #[test]
    fn a_format_7_catalog_without_row_columns_is_refused_by_name_without_rewriting_it() {
        let catalog = temp("format-7.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 1, 1, [9, 8, 7])
            .unwrap();
        drop(service);
        let connection = Connection::open(&catalog).unwrap();
        connection
            .execute_batch(
                "ALTER TABLE entries DROP COLUMN label;
                 ALTER TABLE entries DROP COLUMN actor;
                 ALTER TABLE entries DROP COLUMN timestamp_ms;
                 ALTER TABLE entries DROP COLUMN restore_target_id;
                 PRAGMA user_version=7;",
            )
            .unwrap();
        drop(connection);
        let before = std::fs::read(&catalog).unwrap();
        let error = EditorService::open(&catalog).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(
            error.detail,
            "catalog format 7 is not supported; expected 10; choose a new catalog path"
        );
        assert_eq!(
            std::fs::read(&catalog).unwrap(),
            before,
            "a refused catalog is left byte for byte as it was"
        );
        std::fs::remove_file(catalog).unwrap();
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
        strokes: &[crate::path::Stroke],
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
        let mut drawn: Vec<Vec<crate::path::Stroke>> = Vec::new();
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
    /// Scope: `lightwell-core`'s own catalog, on the JPEG fixture, counting the bytes of every
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
    /// what else the host is doing; the one timed figure a session produces, reopen, is measured
    /// separately and quoted with its load average.
    #[derive(Clone, Copy)]
    struct Growth {
        strokes: usize,
        /// The bytes of every stored entry's JSON: the snapshots, which is where the growth is.
        entries: usize,
        /// The bytes of the content-addressed store: each distinct stroke once.
        store: usize,
        /// The catalog file on disk, which also carries the asset, the page overhead and the index.
        catalog: u64,
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

    /// The host's one-minute load average, so every timed figure below can be quoted with the state
    /// of the machine that produced it. Byte counts do not need it and are not quoted with it.
    fn load_average() -> f64 {
        std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .ok()
            .and_then(|out| String::from_utf8(out.stdout).ok())
            .and_then(|text| {
                text.split_whitespace()
                    .nth(1)
                    .and_then(|first| first.parse().ok())
            })
            .unwrap_or(f64::NAN)
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
    /// The returned asset is the painted one, so a caller can time reopening the catalog it left.
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
            addresses.push(table.insert(one).to_string());
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
            let tx = connection.transaction().unwrap();
            registry.validate_recipe(&entry.snapshot.recipe).unwrap();
            insert_entry(&tx, &default_artifact_root(catalog), &entry).unwrap();
            tx.execute(
                "UPDATE asset_state SET current_entry_id=?1, revision=?2 WHERE asset_id=?3",
                params![entry.id.as_str(), revision as i64, asset.as_str()],
            )
            .unwrap();
            tx.commit().unwrap();
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
                    catalog: std::fs::metadata(catalog).map(|m| m.len()).unwrap_or(0),
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

    /// The independent, larger-scale confirmation of the store's figures: a painting session run to
    /// the per-recipe ceiling on 24 MP and 60 MP, sampled every fifty strokes, with the curve it
    /// traces, the counterfactual beside it, and the time to reopen the catalog it left.
    ///
    /// Peak process memory belongs to the process, so a run measures one source at a time: set
    /// `LIGHTWELL_MASK_GROWTH_SOURCE` to a fixture's path and wrap the run in `/usr/bin/time -l`,
    /// which is where the recorded peak resident set comes from. Without it both sources run in one
    /// process and only the byte counts are attributable.
    ///
    /// Printed rather than asserted, because a timing gate does not belong in the test suite; the
    /// tests below hold the design's figures, the ceiling and the bound in place.
    ///
    /// ```text
    /// cargo test --release --package lightwell-core --lib measure_mask_growth -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "a measurement, not a gate"]
    fn measure_mask_growth_across_a_painting_session() {
        let generated = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/generated");
        let chosen = std::env::var("LIGHTWELL_MASK_GROWTH_SOURCE").ok();
        let sources: Vec<(String, PathBuf)> = match &chosen {
            Some(path) => vec![(path.clone(), PathBuf::from(path))],
            None => ["24mp.jpg", "60mp.jpg"]
                .iter()
                .map(|name| ((*name).to_owned(), generated.join(name)))
                .filter(|(_, path)| path.exists())
                .collect(),
        };
        assert!(
            !sources.is_empty(),
            "generate fixtures first: cargo xtask generate-fixtures --output fixtures/generated"
        );
        println!(
            "load average at the start of this run: {:.2}",
            load_average()
        );
        for (name, source) in &sources {
            let catalog = temp("mask-growth-measured.sqlite");
            let (curve, asset) = painting_session(&catalog, source, CEILING, 50);
            println!(
                "\n{name}: strokes, stored entries + store (MB), catalog file (MB), embedded \
                 counterfactual (MB), factor"
            );
            for point in &curve {
                println!(
                    "  {:>4}  {:>8.3}  {:>8.3}  {:>9.3}  {:>5.1}x",
                    point.strokes,
                    point.stored() as f64 / 1e6,
                    point.catalog as f64 / 1e6,
                    point.embedded as f64 / 1e6,
                    point.embedded as f64 / point.stored() as f64,
                );
            }
            let (a, b, c) = quadratic_fit(&curve);
            let at = |n: usize| {
                curve
                    .iter()
                    .find(|point| point.strokes == n)
                    .copied()
                    .expect("a sampled stroke count")
            };
            println!(
                "  fit S(n) = {a:.4}·n² + {b:.1}·n + {c:.0} bytes; S(1000)/S(500) = {:.2} and \
                 S(500)/S(250) = {:.2} (4 is quadratic, 2 would be linear)",
                at(1000).stored() as f64 / at(500).stored() as f64,
                at(500).stored() as f64 / at(250).stored() as f64,
            );
            println!(
                "  one stroke serializes to {} bytes; the ceiling's {CEILING} distinct strokes hold \
                 {} KiB",
                at(CEILING).store / CEILING,
                at(CEILING).store / 1024,
            );
            for round in 0..3 {
                let started = Instant::now();
                let service = EditorService::open(&catalog).unwrap();
                let state = service.state(&asset).unwrap();
                let elapsed = started.elapsed();
                println!(
                    "  reopen {round}: {:.1} ms at load {:.2} ({} masks, {} strokes resolved)",
                    elapsed.as_secs_f64() * 1e3,
                    load_average(),
                    state.current_entry.snapshot.recipe.masks.len(),
                    state
                        .current_entry
                        .snapshot
                        .recipe
                        .strokes
                        .strokes()
                        .count(),
                );
            }
            std::fs::remove_file(&catalog).unwrap();
        }
    }

    /// The growth is quadratic, and its square term is the one the design records.
    ///
    /// Scope: `lightwell-core`'s own catalog on the 24 MP generated fixture when it has been
    /// generated and on the small JPEG fixture otherwise — the catalog's bytes do not depend on the
    /// source's pixel dimensions, which the measurement above confirms by measuring both — one
    /// stroke of 100 positions per history entry, counting the bytes of every stored entry plus the
    /// bytes of the stroke store. Byte counts, so no load average applies.
    ///
    /// It gates the shape at 400 strokes and leaves the thousand-stroke and ceiling figures to the
    /// measurement above, so the suite does not carry a twenty-second session to learn what four
    /// hundred strokes already say.
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
        // The measurement paints to CEILING, so CEILING has to be a session that packs, and the
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
    fn tiny_stroke(index: usize) -> crate::path::Stroke {
        let x = 0.1 + (index % 4096) as f64 / 16384.0;
        let y = 0.1 + (index / 4096) as f64 / 16384.0;
        crate::path::Stroke::capture(&[[x, y]], 0.04, 50.0, 100.0, false).expect("a legal stroke")
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
        let at_bound: Vec<crate::path::Stroke> = (0..STROKES_PER_MASK).map(stroke).collect();
        let points: usize = at_bound.iter().map(crate::path::Stroke::point_count).sum();
        let recipe = |strokes: &[crate::path::Stroke]| {
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
            .map(crate::path::Stroke::point_count)
            .sum::<usize>()
            <= crate::POINTS_PER_MASK
        {
            over.push(stroke(over.len()));
        }
        let total: usize = over.iter().map(crate::path::Stroke::point_count).sum();
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

    #[test]
    fn a_format_5_catalog_is_refused_by_name_and_left_untouched() {
        let catalog = temp("format-5.sqlite");
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        service
            .apply_pixel(&asset, mutation(0, "pixel"), 1, 1, [9, 8, 7])
            .unwrap();
        drop(service);
        // A format 5 catalog is this schema without the catalog identity and the artifact tables.
        let connection = Connection::open(&catalog).unwrap();
        connection
            .execute_batch(
                "DROP TRIGGER artifact_refs_are_permanent;
                 DROP TABLE artifact_refs;
                 DROP TABLE artifacts;
                 DROP TABLE catalog_meta;
                 PRAGMA user_version=5;",
            )
            .unwrap();
        drop(connection);
        let before = std::fs::read(&catalog).unwrap();
        let error = EditorService::open(&catalog).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Incompatible);
        assert_eq!(
            error.detail,
            "catalog format 5 is not supported; expected 10; choose a new catalog path"
        );
        assert_eq!(
            std::fs::read(&catalog).unwrap(),
            before,
            "the refused catalog keeps every byte, and no artifact directory is created"
        );
        let stem = catalog.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(!catalog.with_file_name(format!("{stem}.artifacts")).exists());
        std::fs::remove_file(catalog).unwrap();
    }
}
