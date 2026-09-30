//! The journal of library changes, and undo and redo over it.
//!
//! Every library change — a pick or clear, a catalog folder or collection change, a Develop, a
//! relink, a removal, an indexed folder — goes through [`apply`], in the caller's one catalog
//! transaction: each item's value is read, set to what the change wants, and recorded with its value
//! before and after (`library_change_rows`) under one numbered change naming the actor, request,
//! method and a label (`library_changes`). An item already holding its value is left out, so a
//! change that changes nothing records nothing. A change covers at most [`MAX_LIBRARY_BATCH`]
//! items; more is `resource-limit`, never split.
//!
//! **Undo and redo are generic.** A change's rows hold everything its inverse needs: [`undo`]
//! writes each row's `before` back, in reverse order, and records that as a new change that
//! `undoes` it; [`redo`] does the same to an undo. Neither knows what kind of change it reverts.
//!
//! - **Whose.** The actor of the request is the undo scope (`client_key`), so a client undoes only
//!   its own changes, and its undo survives a restart. Undo reverts the actor's latest change or
//!   redo not yet undone; redo reverts the actor's latest undo not yet redone, made since its
//!   latest new change (a new change ends what can be redone, as in an editor).
//! - **When not.** An undo (or redo) is refused with `conflict`, naming the items, when any of them
//!   changed since the change it reverts: a later change touched it that is still in effect (a
//!   later change and its undo, or a later undo and its redo, cancel out), or its value is no
//!   longer the one that change left, whoever changed it.
//! - **Retries.** A request is found again by its actor, request identity and method ([`find`]),
//!   durably, so a retried request answers with the change its first attempt recorded, after a
//!   restart too, and never changes anything twice.
//!
//! The journal is append-only; the schema refuses updating or deleting its rows, undoing a change
//! twice and redoing an undo twice.
use super::items;
use crate::{
    AssetId, Error, MutationOutcome,
    catalog_types::{
        LibraryAnswer, LibraryChange, LibraryChangeDetail, LibraryChangeRow, LibraryChangeSeq,
        LibraryItem, LibraryJournal, MAX_LIBRARY_BATCH,
    },
    editor::{decode, encode, now_ms},
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::HashSet;

/// How many items a refused undo names in its error's data; its message names the first.
const NAMED_ITEMS: usize = 100;

/// Who asks for a library change, and through which method.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Request<'a> {
    pub method: &'a str,
    /// The envelope's actor: who the change is attributed to, and whose undo it is.
    pub actor: &'a str,
    pub request_id: &'a str,
}

impl<'a> Request<'a> {
    pub(crate) fn new(method: &'a str, mutation: &'a crate::MutationRequest) -> Self {
        Self {
            method,
            actor: &mutation.actor,
            request_id: &mutation.request_id,
        }
    }
}

/// What one item of a change is to become.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Desired {
    /// This value, `None` removing the item.
    Value(Option<Value>),
    /// The value it has, when it has one; otherwise this. A pick of a file already picked keeps
    /// who picked it and when, and so changes nothing.
    UnlessPresent(Value),
}

/// What a library write did.
#[derive(Debug)]
pub(crate) enum Outcome {
    /// Every item already had its value: nothing was recorded.
    NoOp,
    /// A change was recorded, now or, for a retry, by the request's first attempt.
    Recorded {
        change: LibraryChange,
        /// The photographs whose original the change moved, whose cached rows the service reads
        /// again once it commits.
        sources: Vec<AssetId>,
        deduplicated: bool,
    },
}

impl Outcome {
    /// What the method answers.
    pub(crate) fn answer(&self) -> LibraryAnswer {
        match self {
            Self::NoOp => LibraryAnswer {
                outcome: MutationOutcome::NoOp,
                change: None,
                items: 0,
                deduplicated: false,
            },
            Self::Recorded {
                change,
                deduplicated,
                ..
            } => LibraryAnswer {
                outcome: MutationOutcome::Applied,
                change: Some(change.sequence),
                items: change.item_count,
                deduplicated: *deduplicated,
            },
        }
    }

    /// The change this write recorded now, which the owner announces; none for a no-op or a
    /// retry, whose first attempt announced it.
    pub(crate) fn announced(&self) -> Option<LibraryChangeSeq> {
        match self {
            Self::Recorded {
                change,
                deduplicated: false,
                ..
            } => Some(change.sequence),
            _ => None,
        }
    }

    /// The photographs whose original this write moved.
    pub(crate) fn sources(&self) -> &[AssetId] {
        match self {
            Self::Recorded { sources, .. } => sources,
            Self::NoOp => &[],
        }
    }

    fn deduplicated(change: LibraryChange) -> Self {
        Self::Recorded {
            change,
            sources: Vec::new(),
            deduplicated: true,
        }
    }
}

/// The change a request recorded, if it recorded one: the first change of this actor with this
/// request identity and method.
pub(crate) fn find(
    connection: &Connection,
    request: Request<'_>,
) -> Result<Option<LibraryChange>, Error> {
    Ok(connection
        .prepare_cached(&format!(
            "SELECT {CHANGE_COLUMNS} FROM library_changes c
             WHERE c.request_id = ?1 AND c.client_key = ?2 AND c.method = ?3
             ORDER BY c.sequence LIMIT 1"
        ))?
        .query_row(
            params![request.request_id, request.actor, request.method],
            read_change,
        )
        .optional()?)
}

/// Record one library change in the caller's transaction: set each item to what it is to become,
/// leaving out the ones that already are, and record the rest with their values before and after,
/// labelled by `label` from the rows it records. An item named twice counts once, as first named.
/// A retry of a request that recorded a change answers that change and writes nothing.
pub(crate) fn apply(
    tx: &Transaction<'_>,
    request: Request<'_>,
    changes: Vec<(LibraryItem, Desired)>,
    label: impl FnOnce(&[LibraryChangeRow]) -> String,
) -> Result<Outcome, Error> {
    if let Some(change) = find(tx, request)? {
        return Ok(Outcome::deduplicated(change));
    }
    if changes.len() > MAX_LIBRARY_BATCH {
        return Err(batch_limit(changes.len()));
    }
    let mut named = HashSet::with_capacity(changes.len());
    let mut rows = Vec::with_capacity(changes.len());
    for (item, desired) in changes {
        if !named.insert(item.clone()) {
            continue;
        }
        let before = items::read(tx, &item)?;
        let after = match desired {
            Desired::Value(value) => value,
            Desired::UnlessPresent(value) => Some(before.clone().unwrap_or(value)),
        };
        if before == after {
            continue;
        }
        items::write(tx, &item, after.as_ref())?;
        rows.push(LibraryChangeRow {
            item,
            before,
            after,
        });
    }
    if rows.is_empty() {
        return Ok(Outcome::NoOp);
    }
    let label = label(&rows);
    record(tx, request, &label, Relation::None, rows)
}

/// Revert the actor's latest change (or redo) not yet undone, by appending its inverse; a no-op
/// when there is none. Refused, naming the items, when any of them changed since.
pub(crate) fn undo(tx: &Transaction<'_>, request: Request<'_>) -> Result<Outcome, Error> {
    if let Some(change) = find(tx, request)? {
        return Ok(Outcome::deduplicated(change));
    }
    let latest: Option<u64> = tx
        .prepare_cached(
            "SELECT c.sequence FROM library_changes c
             WHERE c.client_key = ?1 AND c.undoes IS NULL
                 AND NOT EXISTS (SELECT 1 FROM library_changes u WHERE u.undoes = c.sequence)
             ORDER BY c.sequence DESC LIMIT 1",
        )?
        .query_row([request.actor], |row| {
            row.get::<_, i64>(0).map(|sequence| sequence as u64)
        })
        .optional()?;
    let Some(target) = latest else {
        return Ok(Outcome::NoOp);
    };
    let label = format!("Undo {}", base_label(tx, target)?);
    revert(tx, request, target, Relation::Undoes(target), &label)
}

/// Revert the actor's latest undo not yet redone, made since its latest new change, by appending
/// its inverse; a no-op when there is none. Refused as undo is.
pub(crate) fn redo(tx: &Transaction<'_>, request: Request<'_>) -> Result<Outcome, Error> {
    if let Some(change) = find(tx, request)? {
        return Ok(Outcome::deduplicated(change));
    }
    let latest: Option<u64> = tx
        .prepare_cached(
            "SELECT u.sequence FROM library_changes u
             WHERE u.client_key = ?1 AND u.undoes IS NOT NULL
                 AND NOT EXISTS (SELECT 1 FROM library_changes r WHERE r.redoes = u.sequence)
                 AND u.sequence > (SELECT ifnull(max(f.sequence), 0) FROM library_changes f
                     WHERE f.client_key = ?1 AND f.undoes IS NULL AND f.redoes IS NULL)
             ORDER BY u.sequence DESC LIMIT 1",
        )?
        .query_row([request.actor], |row| {
            row.get::<_, i64>(0).map(|sequence| sequence as u64)
        })
        .optional()?;
    let Some(target) = latest else {
        return Ok(Outcome::NoOp);
    };
    let label = format!("Redo {}", base_label(tx, target)?);
    revert(tx, request, target, Relation::Redoes(target), &label)
}

/// Changes after `after`, oldest first, at most `limit`, with the cursor that continues them when
/// there are more.
pub(crate) fn page(
    connection: &Connection,
    after: Option<u64>,
    limit: usize,
) -> Result<LibraryJournal, Error> {
    let mut changes = connection
        .prepare_cached(&format!(
            "SELECT {CHANGE_COLUMNS} FROM library_changes c WHERE c.sequence > ?1
             ORDER BY c.sequence LIMIT ?2"
        ))?
        .query_map(
            params![sql_sequence(after.unwrap_or(0)), limit as i64 + 1],
            read_change,
        )?
        .collect::<Result<Vec<_>, _>>()?;
    let next_after = (changes.len() > limit).then(|| {
        changes.truncate(limit);
        changes.last().map(|change| change.sequence)
    });
    Ok(LibraryJournal {
        changes,
        next_after: next_after.flatten(),
    })
}

/// One change with each item's value before and after, in the order it applied them.
pub(crate) fn inspect(
    connection: &Connection,
    sequence: u64,
) -> Result<LibraryChangeDetail, Error> {
    let change = change(connection, sequence)?;
    let rows = rows_of(connection, sequence)?;
    Ok(LibraryChangeDetail { change, rows })
}

/// The newest change's sequence, or 0 before the first: what a view evaluated now has seen.
pub(crate) fn latest(connection: &Connection) -> Result<LibraryChangeSeq, Error> {
    let sequence: i64 = connection
        .prepare_cached("SELECT ifnull(max(sequence), 0) FROM library_changes")?
        .query_row([], |row| row.get(0))?;
    Ok(LibraryChangeSeq(sequence as u64))
}

/// What a recorded change reverts.
#[derive(Clone, Copy)]
enum Relation {
    None,
    Undoes(u64),
    Redoes(u64),
}

/// The columns [`read_change`] maps: the change and the undo that undid it, if any.
const CHANGE_COLUMNS: &str = "c.sequence, c.actor, c.request_id, c.method, c.label, c.time_ms,
    c.item_count, c.undoes, c.redoes,
    (SELECT u.sequence FROM library_changes u WHERE u.undoes = c.sequence)";

fn read_change(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryChange> {
    let sequence = |index| -> rusqlite::Result<Option<LibraryChangeSeq>> {
        Ok(row
            .get::<_, Option<i64>>(index)?
            .map(|sequence| LibraryChangeSeq(sequence as u64)))
    };
    Ok(LibraryChange {
        sequence: LibraryChangeSeq(row.get::<_, i64>(0)? as u64),
        actor: row.get(1)?,
        request_id: row.get(2)?,
        method: row.get(3)?,
        label: row.get(4)?,
        time_ms: row.get(5)?,
        item_count: row.get(6)?,
        undoes: sequence(7)?,
        redoes: sequence(8)?,
        undone_by: sequence(9)?,
    })
}

/// A sequence a client handed back, as the catalog stores it; past the catalog's range it names
/// nothing, which every query here answers as such.
fn sql_sequence(sequence: u64) -> i64 {
    i64::try_from(sequence).unwrap_or(i64::MAX)
}

fn change(connection: &Connection, sequence: u64) -> Result<LibraryChange, Error> {
    connection
        .prepare_cached(&format!(
            "SELECT {CHANGE_COLUMNS} FROM library_changes c WHERE c.sequence = ?1"
        ))?
        .query_row([sql_sequence(sequence)], read_change)
        .optional()?
        .ok_or_else(|| Error::validation(format!("unknown library change {sequence}")))
}

/// A change's rows in the order it applied them.
fn rows_of(connection: &Connection, sequence: u64) -> Result<Vec<LibraryChangeRow>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT item_kind, item_key, before_json, after_json FROM library_change_rows
         WHERE change_seq = ?1 ORDER BY ordinal",
    )?;
    let stored = statement
        .query_map([sql_sequence(sequence)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    stored
        .into_iter()
        .map(|(kind, key, before, after)| {
            let value = |stored: Option<String>| {
                stored
                    .map(|text| decode::<Value>("stored library value", text))
                    .transpose()
            };
            Ok(LibraryChangeRow {
                item: LibraryItem::from_row(&kind, &key)?,
                before: value(before)?,
                after: value(after)?,
            })
        })
        .collect()
}

/// The label of the change a chain of undos and redos starts from.
fn base_label(connection: &Connection, mut sequence: u64) -> Result<String, Error> {
    loop {
        let change = change(connection, sequence)?;
        match change.undoes.or(change.redoes) {
            Some(earlier) => sequence = earlier.0,
            None => return Ok(change.label),
        }
    }
}

/// Write `target`'s inverse and record it as `relation`, after checking nothing it touched has
/// changed since.
fn revert(
    tx: &Transaction<'_>,
    request: Request<'_>,
    target: u64,
    relation: Relation,
    label: &str,
) -> Result<Outcome, Error> {
    let rows = rows_of(tx, target)?;
    let changed = changed_since(tx, target, &rows)?;
    if !changed.is_empty() {
        return Err(refusal(label, &changed));
    }
    let inverse: Vec<LibraryChangeRow> = rows
        .into_iter()
        .rev()
        .map(|row| LibraryChangeRow {
            item: row.item,
            before: row.after,
            after: row.before,
        })
        .collect();
    for row in &inverse {
        items::write(tx, &row.item, row.after.as_ref())?;
    }
    record(tx, request, label, relation, inverse)
}

/// The items of `target` that changed since it: those a later change still in effect touched, then
/// those whose value is not the one it left, each once, in its order.
fn changed_since(
    tx: &Transaction<'_>,
    target: u64,
    rows: &[LibraryChangeRow],
) -> Result<Vec<LibraryItem>, Error> {
    // Every later row of each of its items, found through the journal's item index.
    let later: Vec<(String, String, u64)> = tx
        .prepare_cached(
            "SELECT later.item_kind, later.item_key, later.change_seq
             FROM library_change_rows mine
             JOIN library_change_rows later ON later.item_kind = mine.item_kind
                 AND later.item_key = mine.item_key AND later.change_seq > mine.change_seq
             WHERE mine.change_seq = ?1",
        )?
        .query_map([sql_sequence(target)], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? as u64))
        })?
        .collect::<Result<_, _>>()?;
    let mut touched: HashSet<(String, String)> = HashSet::new();
    if !later.is_empty() {
        // A later change reverted by a later one cancels out with it: a change and its undo, an
        // undo and its redo. Walking them oldest first, a change that reverts one still in effect
        // takes it out; any other is in effect. What is left touched the items.
        let mut sequences: Vec<u64> = later.iter().map(|(_, _, sequence)| *sequence).collect();
        sequences.sort_unstable();
        sequences.dedup();
        let mut reverts = tx.prepare_cached(
            "SELECT ifnull(undoes, redoes) FROM library_changes WHERE sequence = ?1",
        )?;
        let mut in_effect = HashSet::new();
        for sequence in sequences {
            let reverted: Option<i64> = reverts
                .query_row([sql_sequence(sequence)], |row| row.get(0))
                .optional()?
                .flatten();
            let cancels = reverted.is_some_and(|reverted| in_effect.remove(&(reverted as u64)));
            if !cancels {
                in_effect.insert(sequence);
            }
        }
        touched = later
            .into_iter()
            .filter(|(_, _, sequence)| in_effect.contains(sequence))
            .map(|(kind, key, _)| (kind, key))
            .collect();
    }
    let mut changed = Vec::new();
    for row in rows {
        let key = (row.item.kind().to_owned(), row.item.key());
        if touched.contains(&key) || items::read(tx, &row.item)? != row.after {
            changed.push(row.item.clone());
        }
    }
    Ok(changed)
}

/// The refusal of an undo or redo whose items changed since, naming them.
fn refusal(label: &str, changed: &[LibraryItem]) -> Error {
    let first = &changed[0];
    let others = match changed.len() - 1 {
        0 => String::new(),
        1 => " and 1 other item".into(),
        more => format!(" and {more} other items"),
    };
    Error::conflict(format!(
        "{label} is refused: {} {}{others} changed since",
        first.kind(),
        first.key()
    ))
    .with_data(json!({
        "items": changed.iter().take(NAMED_ITEMS).collect::<Vec<_>>(),
        "count": changed.len(),
    }))
}

fn batch_limit(count: usize) -> Error {
    Error::resource_limit(format!(
        "{count} items exceed the {MAX_LIBRARY_BATCH} a library change covers"
    ))
}

/// Append one change and its rows.
fn record(
    tx: &Transaction<'_>,
    request: Request<'_>,
    label: &str,
    relation: Relation,
    rows: Vec<LibraryChangeRow>,
) -> Result<Outcome, Error> {
    if rows.len() > MAX_LIBRARY_BATCH {
        return Err(batch_limit(rows.len()));
    }
    let (undoes, redoes) = match relation {
        Relation::None => (None, None),
        Relation::Undoes(target) => (Some(sql_sequence(target)), None),
        Relation::Redoes(target) => (None, Some(sql_sequence(target))),
    };
    let time_ms = now_ms();
    tx.prepare_cached(
        "INSERT INTO library_changes (actor, client_key, request_id, method, label, time_ms,
             item_count, undoes, redoes)
         VALUES (?1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?
    .execute(params![
        request.actor,
        request.request_id,
        request.method,
        label,
        time_ms,
        rows.len() as i64,
        undoes,
        redoes,
    ])?;
    let sequence = tx.last_insert_rowid();
    let mut insert = tx.prepare_cached(
        "INSERT INTO library_change_rows (change_seq, ordinal, item_kind, item_key, before_json,
             after_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut sources = Vec::new();
    for (ordinal, row) in rows.iter().enumerate() {
        insert.execute(params![
            sequence,
            ordinal as i64,
            row.item.kind(),
            row.item.key(),
            row.before.as_ref().map(encode).transpose()?,
            row.after.as_ref().map(encode).transpose()?,
        ])?;
        if let LibraryItem::AssetSource { asset_id } = &row.item {
            sources.push(asset_id.clone());
        }
    }
    let (undoes, redoes) = match relation {
        Relation::None => (None, None),
        Relation::Undoes(target) => (Some(LibraryChangeSeq(target)), None),
        Relation::Redoes(target) => (None, Some(LibraryChangeSeq(target))),
    };
    Ok(Outcome::Recorded {
        change: LibraryChange {
            sequence: LibraryChangeSeq(sequence as u64),
            actor: request.actor.to_owned(),
            request_id: request.request_id.to_owned(),
            method: request.method.to_owned(),
            label: label.to_owned(),
            time_ms,
            item_count: rows.len() as u32,
            undoes,
            redoes,
            undone_by: None,
        },
        sources,
        deduplicated: false,
    })
}

/// Whether the refusal named `item`; for tests reading an error's data.
#[cfg(test)]
pub(crate) fn names(error: &Error, item: &LibraryItem) -> bool {
    error.data.as_ref().is_some_and(|data| {
        data["items"]
            .as_array()
            .is_some_and(|items| items.contains(&serde_json::to_value(item).unwrap()))
    })
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
