//! Removing photographs from the catalog, putting them back, and emptying Removed
//! (`docs/design/catalog.md`, "Removing", P12).
//!
//! - **Removing** (`asset.remove`) and **putting back** (`asset.restore`) are one library change
//!   each, of `asset-removal` items: a removed photograph's `removed_ms` says when it was removed,
//!   and putting it back clears it. Nothing else about it changes — its edits, history, versions,
//!   collections and catalog folder stay as they were — and nothing on disk does. Every source but
//!   Removed, and every count, leaves a removed photograph out. The journal undoes and redoes
//!   either like any other change.
//! - **Emptying Removed** (`catalog.empty-removed`, [`empty`]) is the one destructive catalog
//!   operation: in one transaction it deletes every removed photograph's catalog record — entries,
//!   state, requests, versions, capture row, collection memberships, artifact references and asset
//!   row — and the strokes and artifact rows nothing left in the catalog names. It is not a library
//!   change, since nothing it deletes can be restored, and it leaves the journal as it is: an undo
//!   or redo that names a deleted photograph finds its value gone and is refused. Beside sending an
//!   unedited photograph back, which deletes a record holding only its Original entry (P9), it is
//!   the only path that deletes a history entry.
use crate::{
    AssetId, Error,
    artifacts::{ArtifactId, LiveArtifacts},
    catalog_types::{AssetRemovalValue, AssetRowId, LibraryChangeRow, LibraryItem},
    editor::library_rows,
    library::{
        folders::counted,
        journal::Desired,
        tree::{self, Planned},
    },
    path::STROKES_FIELD,
};
use rusqlite::{Connection, OptionalExtension, Transaction};
use std::collections::BTreeSet;

/// Remove `assets` from the catalog as of `now_ms`: one `asset-removal` item each. A photograph
/// already removed keeps when it was removed, and so changes nothing.
pub(crate) fn remove(assets: Vec<(AssetId, AssetRowId)>, now_ms: i64) -> Result<Planned, Error> {
    let removed = serde_json::to_value(AssetRemovalValue { removed_ms: now_ms })
        .map_err(|error| Error::internal(format!("cannot encode a removal: {error}")))?;
    let changes = assets
        .into_iter()
        .map(|(asset_id, _)| {
            (
                LibraryItem::AssetRemoval { asset_id },
                Desired::UnlessPresent(removed.clone()),
            )
        })
        .collect();
    Ok(Planned::counted(changes, |connection, rows| {
        format!("Removed {}", photographs(connection, rows))
    }))
}

/// Put `assets` back: their `asset-removal` items go absent. One that is not removed changes
/// nothing.
pub(crate) fn restore(assets: Vec<(AssetId, AssetRowId)>) -> Planned {
    let changes = assets
        .into_iter()
        .map(|(asset_id, _)| (LibraryItem::AssetRemoval { asset_id }, Desired::Value(None)))
        .collect();
    Planned::counted(changes, |connection, rows| {
        format!("Put back {}", photographs(connection, rows))
    })
}

/// The photographs a change's rows name, as its label says them: "DSC_0412.NEF" or
/// "5 photographs".
fn photographs(connection: &Connection, rows: &[LibraryChangeRow]) -> String {
    tree::photographs(connection, rows, |count| {
        counted(count, "photograph", "photographs")
    })
}

/// What one emptying deleted, in its one transaction.
#[derive(Debug, Default)]
pub(crate) struct Emptied {
    /// The photographs whose records it deleted, earliest removed first.
    pub assets: Vec<AssetId>,
    /// The artifact rows only their entries referenced, none of them published by this process:
    /// their files are left for a collection to remove.
    pub artifacts: Vec<ArtifactId>,
    /// The removed photographs past the bound, still in Removed for a later emptying.
    pub remaining: u64,
}

/// The triggers that stand in the way of deleting a photograph's records, which [`empty`] lifts
/// inside its own transaction only: the artifact references' permanence and the collapsed
/// entries' — an entry auto-collapse hid is part of its photograph's record and goes with it. An
/// entry's and a stroke's immutability refuse an update, not a deletion, and the journal, whose
/// rows are the other permanent ones, is left as it is.
pub(crate) const LIFTED_TRIGGERS: &[&str] = &[
    "artifact_refs_are_permanent",
    "collapsed_entries_are_permanent",
];

/// Empty Removed in the caller's transaction, at most `limit` photographs, earliest removed first:
/// delete each one's catalog record, the entries auto-collapse hid with it, then every stroke its
/// entries named that no remaining entry names, then every artifact row its entries referenced
/// that no remaining entry references and no task of this process published (`live`). The
/// [`LIFTED_TRIGGERS`] are lifted for the deletion and created again, from the SQL the catalog
/// stored for them, before this answers; a failure anywhere rolls the whole transaction back, the
/// triggers with it. Nothing on disk is touched and the journal is left as it is.
///
/// Its cost is SQL on the owner: one indexed delete per table for the photographs together, one
/// read of their entries' text, and, only when those entries named a stroke the store holds, one
/// read of every other entry's text that names any stroke, since a stroke carries no count of the
/// entries that name it and two photographs share a stroke painted alike.
pub(crate) fn empty(
    tx: &Transaction<'_>,
    live: &LiveArtifacts,
    limit: usize,
) -> Result<Emptied, Error> {
    let (assets, removed) = removed_photographs(tx, limit)?;
    if assets.is_empty() {
        return Ok(Emptied::default());
    }
    let ids = serde_json::to_string(&assets)
        .map_err(|error| Error::internal(format!("cannot encode photographs: {error}")))?;
    let mut strokes = stored_strokes(tx, &ids)?;
    let referenced = referenced_artifacts(tx, &ids)?;
    lifted(tx, LIFTED_TRIGGERS, |tx| {
        library_rows::delete_photographs(tx, &assets)
    })?;
    if !strokes.is_empty() {
        still_named(tx, &mut strokes)?;
    }
    let mut delete_stroke = tx.prepare_cached("DELETE FROM strokes WHERE id = ?1")?;
    for stroke in &strokes {
        delete_stroke.execute([stroke])?;
    }
    let mut delete_artifact = tx.prepare_cached(
        "DELETE FROM artifacts WHERE id = ?1
             AND NOT EXISTS (SELECT 1 FROM artifact_refs WHERE artifact_id = ?1)",
    )?;
    let mut artifacts = Vec::new();
    for artifact in referenced {
        if !live.contains(&artifact) && delete_artifact.execute([artifact.as_str()])? > 0 {
            artifacts.push(artifact);
        }
    }
    let remaining = removed - assets.len() as u64;
    Ok(Emptied {
        assets,
        artifacts,
        remaining,
    })
}

/// The removed photographs, earliest removed first, at most `limit`, and how many are removed in
/// all. One range of the removal index.
fn removed_photographs(
    connection: &Connection,
    limit: usize,
) -> Result<(Vec<AssetId>, u64), Error> {
    let assets = connection
        .prepare_cached(
            "SELECT id FROM assets WHERE removed_ms IS NOT NULL
             ORDER BY removed_ms, row_id LIMIT ?1",
        )?
        .query_map([limit as i64], |row| row.get::<_, String>(0))?
        .map(|id| AssetId::parse(id?))
        .collect::<Result<Vec<_>, Error>>()?;
    let removed: i64 = connection
        .prepare_cached("SELECT count(*) FROM assets WHERE removed_ms IS NOT NULL")?
        .query_row([], |row| row.get(0))?;
    Ok((assets, removed as u64))
}

/// The strokes the store holds that the entries of the photographs `ids` (a JSON array) may name.
fn stored_strokes(connection: &Connection, ids: &str) -> Result<BTreeSet<String>, Error> {
    let mut named = BTreeSet::new();
    let mut entries = connection.prepare_cached(
        "SELECT entry_json FROM entries
         WHERE asset_id IN (SELECT value FROM json_each(?1)) AND instr(entry_json, ?2) > 0",
    )?;
    let mut texts = entries.query(rusqlite::params![ids, strokes_key()])?;
    while let Some(row) = texts.next()? {
        let text: String = row.get(0)?;
        named.extend(stroke_addresses(&text).map(str::to_owned));
    }
    let mut stored = connection.prepare_cached("SELECT 1 FROM strokes WHERE id = ?1")?;
    let mut held = BTreeSet::new();
    for stroke in named {
        if stored
            .query_row([&stroke], |_| Ok(()))
            .optional()?
            .is_some()
        {
            held.insert(stroke);
        }
    }
    Ok(held)
}

/// Leave out of `strokes` every one an entry still in the catalog names.
fn still_named(connection: &Connection, strokes: &mut BTreeSet<String>) -> Result<(), Error> {
    let mut entries = connection
        .prepare_cached("SELECT entry_json FROM entries WHERE instr(entry_json, ?1) > 0")?;
    let mut texts = entries.query([strokes_key()])?;
    while let Some(row) = texts.next()? {
        let text: String = row.get(0)?;
        for address in stroke_addresses(&text) {
            strokes.remove(address);
        }
        if strokes.is_empty() {
            break;
        }
    }
    Ok(())
}

/// The key a mask component's stroke addresses are under, as an entry's JSON writes it: an entry
/// that names a stroke holds it.
fn strokes_key() -> String {
    format!("\"{STROKES_FIELD}\"")
}

/// The stroke addresses an entry's JSON text may name: every string of 32 lowercase hex digits in
/// it. A stroke reference is always one (`crate::path::StrokeId`), so none is missed; a string of
/// that shape that is not one only keeps a stroke nothing names, never deletes one something does.
pub(crate) fn stroke_addresses(text: &str) -> impl Iterator<Item = &str> {
    text.split('"').filter(|piece| {
        piece.len() == 32
            && piece
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// The artifacts the entries of the photographs `ids` (a JSON array) reference, each once.
fn referenced_artifacts(connection: &Connection, ids: &str) -> Result<Vec<ArtifactId>, Error> {
    connection
        .prepare_cached(
            "SELECT DISTINCT r.artifact_id FROM artifact_refs r JOIN entries e ON e.id = r.entry_id
             WHERE e.asset_id IN (SELECT value FROM json_each(?1)) ORDER BY r.artifact_id",
        )?
        .query_map([ids], |row| row.get::<_, String>(0))?
        .map(|id| ArtifactId::parse(id?))
        .collect()
}

/// Run `f` in the caller's transaction with `triggers` dropped, and create each again, from the
/// SQL the catalog stored for it, before answering. A trigger is lifted for exactly `f` and never
/// outside the transaction: a failure in `f`, or anywhere later, rolls the transaction back with
/// the trigger in it.
pub(crate) fn lifted<T>(
    tx: &Transaction<'_>,
    triggers: &[&str],
    f: impl FnOnce(&Transaction<'_>) -> Result<T, Error>,
) -> Result<T, Error> {
    let mut stored = Vec::with_capacity(triggers.len());
    for name in triggers {
        let sql: Option<String> = tx
            .prepare_cached("SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = ?1")?
            .query_row([name], |row| row.get(0))
            .optional()?;
        let sql =
            sql.ok_or_else(|| Error::incompatible(format!("the catalog has no trigger {name}")))?;
        tx.execute_batch(&format!("DROP TRIGGER \"{name}\""))?;
        stored.push(sql);
    }
    let answer = f(tx)?;
    for sql in &stored {
        tx.execute_batch(sql)?;
    }
    Ok(answer)
}
