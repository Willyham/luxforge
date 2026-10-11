//! The development set the owner holds for every client (`docs/design/catalog.md`, "Develop and
//! the development set"; `docs/design/hdr-merge.md`, "Jobs, commands and client state").
//!
//! - **One current set a session.** `development_sets` marks at most one set current. Replacing it
//!   ([`replace`]) — Develop N's photographs, a catalog view's opened in Develop, one photograph
//!   opened alone, or none for a file opened alone — retires the old set and starts a new one with
//!   its own identity; it is session state, not a library change, so nothing undoes it. Opening the
//!   catalog retires the set the last session left ([`end_session`]).
//! - **Members are journal items.** Adding and removing members (`development-set.add` and
//!   `.remove`), a Pick merged's publication ([`membership_for_publication`]) and Remove and Put
//!   back of a merge's result (`crate::library::merge::lifecycle`) change `set-member` items, each
//!   valued with the member's place. Places only grow ([`SetMemberValue`]), so an undo puts a
//!   member back exactly between the members it was between, whatever joined or left since, and
//!   the journal's per-item checks refuse an undo only when that member itself changed since.
//! - **Revision.** One counter for the catalog (`catalog_meta`'s `development_set_revision`) moves
//!   on with every replacement and every member written, in the same transaction; the set records
//!   the revision of its last change, which `development-set.read` answers and the owner announces
//!   with the change, so a client holding it knows its set is current.
//! - **Retention.** A retired set keeps only what a library change names: the members whose
//!   `set-member` rows the journal holds, which an undo or redo may still write, and the set's row
//!   while the journal names any of its members. The rest goes when the set is retired, so retired
//!   sets are bounded by the journal that references them, as the journal is by its own policy.
//!
//! Everything here is SQL on the owner over the two tables' keys; no pixel or file is read.
use super::journal::Desired;
use crate::{
    AssetId, Error, SourceTag,
    catalog_types::{
        AssetRowId, DevelopmentSetId, DevelopmentSetPage, LibraryItem, MAX_SET_MEMBERS, MergeId,
        SetMember, SetMemberValue,
    },
    editor::now_ms,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde_json::{Value, json};
use std::collections::HashSet;

/// The key of the catalog's development-set revision in `catalog_meta`.
const REVISION_KEY: &str = "development_set_revision";

/// The current set: its row, identity, the revision of its last change and the place its next
/// member takes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CurrentSet {
    pub row: i64,
    pub id: DevelopmentSetId,
    pub revision: u64,
    pub next_position: u64,
}

/// The set a request was made against, captured when it was made: what a merge's publication
/// appends its result to, or, with none, the one-result set it creates. A set replaced since is a
/// named conflict at publication, never an insertion into another set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SetCapture {
    pub set_id: Option<DevelopmentSetId>,
    pub revision: u64,
}

/// The current set, if the session has one. One read of the `current` key.
pub(crate) fn current(connection: &Connection) -> Result<Option<CurrentSet>, Error> {
    let row: Option<(i64, String, i64, i64)> = connection
        .prepare_cached(
            "SELECT row, id, revision, next_position FROM development_sets WHERE current = 1",
        )?
        .query_row([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .optional()?;
    row.map(|(row, id, revision, next_position)| {
        Ok(CurrentSet {
            row,
            id: DevelopmentSetId::parse(id)?,
            revision: revision as u64,
            next_position: next_position as u64,
        })
    })
    .transpose()
}

/// The catalog's development-set revision: 0 before any set was made.
pub(crate) fn revision(connection: &Connection) -> Result<u64, Error> {
    let revision: Option<i64> = connection
        .prepare_cached("SELECT CAST(value AS INTEGER) FROM catalog_meta WHERE key = ?1")?
        .query_row([REVISION_KEY], |row| row.get(0))
        .optional()?;
    Ok(revision.unwrap_or(0).max(0) as u64)
}

/// Move the catalog's development-set revision on, answering the new one.
fn bump(tx: &Transaction<'_>) -> Result<u64, Error> {
    let next = revision(tx)? + 1;
    tx.prepare_cached(
        "INSERT INTO catalog_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )?
    .execute(params![REVISION_KEY, next.to_string()])?;
    Ok(next)
}

/// What a request is made against now: the current set and the revision.
pub(crate) fn capture(connection: &Connection) -> Result<SetCapture, Error> {
    Ok(SetCapture {
        set_id: current(connection)?.map(|set| set.id),
        revision: revision(connection)?,
    })
}

/// Refuse a request naming a set other than the current one: it was replaced since the client
/// last read it, and a change meant for it never lands in another.
pub(crate) fn check_expected(
    current: Option<&CurrentSet>,
    expected: Option<&DevelopmentSetId>,
) -> Result<(), Error> {
    match (expected, current) {
        (Some(expected), Some(set)) if set.id == *expected => Ok(()),
        (Some(expected), _) => Err(replaced(expected, current)),
        (None, _) => Ok(()),
    }
}

fn replaced(expected: &DevelopmentSetId, current: Option<&CurrentSet>) -> Error {
    let now = match current {
        Some(set) => format!("{} is current now", set.id),
        None => "there is no development set now".to_owned(),
    };
    Error::conflict(format!(
        "development set {expected} was replaced; {now}; read the set again"
    ))
    .with_data(json!({
        "set_id": expected,
        "current": current.map(|set| &set.id),
    }))
}

/// Start a new, empty current set, answering it. The caller has retired any other.
fn create(tx: &Transaction<'_>, now: i64) -> Result<CurrentSet, Error> {
    let id = DevelopmentSetId::new();
    let revision = bump(tx)?;
    tx.prepare_cached(
        "INSERT INTO development_sets (id, created_ms, retired_ms, current, revision,
             next_position)
         VALUES (?1, ?2, NULL, 1, ?3, 0)",
    )?
    .execute(params![id.as_str(), now, revision as i64])?;
    Ok(CurrentSet {
        row: tx.last_insert_rowid(),
        id,
        revision,
        next_position: 0,
    })
}

/// Retire the current set, if there is one, keeping only what the journal names of it.
fn retire_current(tx: &Transaction<'_>, now: i64) -> Result<Option<DevelopmentSetId>, Error> {
    let Some(set) = current(tx)? else {
        return Ok(None);
    };
    tx.prepare_cached(
        "UPDATE development_sets SET current = NULL, retired_ms = ?2 WHERE row = ?1",
    )?
    .execute(params![set.row, now])?;
    prune(tx, set.row, &set.id)?;
    Ok(Some(set.id))
}

/// Keep of a retired set only the members a library change names, and its row only while the
/// journal names any of its members. Range reads of the journal's item index.
fn prune(tx: &Transaction<'_>, row: i64, id: &DevelopmentSetId) -> Result<(), Error> {
    let (lower, upper) = keys_of(id);
    tx.prepare_cached(
        "DELETE FROM development_set_members WHERE set_row = ?1 AND asset_row NOT IN (
             SELECT a.row_id FROM library_change_rows r
             JOIN assets a ON a.id = substr(r.item_key, ?4)
             WHERE r.item_kind = 'set-member' AND r.item_key > ?2 AND r.item_key < ?3)",
    )?
    .execute(params![row, lower, upper, lower.len() as i64 + 1])?;
    tx.prepare_cached(
        "DELETE FROM development_sets WHERE row = ?1 AND NOT EXISTS (
             SELECT 1 FROM library_change_rows
             WHERE item_kind = 'set-member' AND item_key > ?2 AND item_key < ?3)",
    )?
    .execute(params![row, lower, upper])?;
    Ok(())
}

/// The range of `set-member` keys of `set`'s members: after `set-…/` and before `set-…0`, the
/// character after the separator.
fn keys_of(set: &DevelopmentSetId) -> (String, String) {
    (format!("{set}/"), format!("{set}0"))
}

/// A catalog session ends with the process that opened it: retire the set the last session left,
/// moving the revision on. Nothing is written when there is none.
pub(crate) fn end_session(connection: &mut Connection) -> Result<(), Error> {
    if current(connection)?.is_none() {
        return Ok(());
    }
    crate::editor::write(connection, |tx| {
        retire_current(tx, now_ms())?;
        bump(tx)?;
        Ok(())
    })
}

/// Replace the current set with `members` in their order, each once (a photograph named again
/// keeps its first place), or with none when `members` is `None`: the old set is retired, and the
/// new one, if any, answered. More than [`MAX_SET_MEMBERS`] is `resource-limit`.
pub(crate) fn replace(
    tx: &Transaction<'_>,
    members: Option<&[AssetRowId]>,
) -> Result<Option<CurrentSet>, Error> {
    let now = now_ms();
    retire_current(tx, now)?;
    let Some(members) = members else {
        bump(tx)?;
        return Ok(None);
    };
    if members.len() > MAX_SET_MEMBERS {
        return Err(Error::resource_limit(format!(
            "{} photographs exceed the {MAX_SET_MEMBERS} a development set holds",
            members.len()
        )));
    }
    let mut set = create(tx, now)?;
    let mut insert = tx.prepare_cached(
        "INSERT INTO development_set_members (set_row, asset_row, position) VALUES (?1, ?2, ?3)
         ON CONFLICT(set_row, asset_row) DO NOTHING",
    )?;
    let mut position = 0i64;
    for member in members {
        position += insert.execute(params![set.row, member.0, position])? as i64;
    }
    set.next_position = position as u64;
    tx.prepare_cached("UPDATE development_sets SET next_position = ?2 WHERE row = ?1")?
        .execute(params![set.row, position])?;
    Ok(Some(set))
}

/// The photographs `assets` names as their rows, refused with `validation` for one the catalog
/// does not hold or holds in Removed. One indexed read each.
pub(crate) fn rows_of(
    connection: &Connection,
    assets: &[AssetId],
) -> Result<Vec<AssetRowId>, Error> {
    let mut statement =
        connection.prepare_cached("SELECT row_id, removed_ms FROM assets WHERE id = ?1")?;
    assets
        .iter()
        .map(|asset| {
            let row: Option<(i64, Option<i64>)> = statement
                .query_row([asset.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()?;
            match row {
                None => Err(Error::validation(format!("unknown asset {asset}"))),
                Some((_, Some(_))) => Err(Error::validation(format!(
                    "photograph {asset} is in Removed; put it back first"
                ))),
                Some((row, None)) => Ok(AssetRowId(row)),
            }
        })
        .collect()
}

/// The `set-member` changes that add `assets` to `set` after its members, each once, in order: a
/// member already in it keeps its place.
pub(crate) fn additions(
    set: &CurrentSet,
    assets: &[AssetId],
) -> Result<Vec<(LibraryItem, Desired)>, Error> {
    let mut named = HashSet::with_capacity(assets.len());
    let mut changes = Vec::with_capacity(assets.len());
    let mut position = set.next_position;
    for asset in assets {
        if !named.insert(asset) {
            continue;
        }
        changes.push((
            item(&set.id, asset),
            Desired::UnlessPresent(member_value(position)?),
        ));
        position += 1;
    }
    Ok(changes)
}

/// The `set-member` changes that take `assets` out of `set`; a photograph not in it changes
/// nothing.
pub(crate) fn removals(set: &CurrentSet, assets: &[AssetId]) -> Vec<(LibraryItem, Desired)> {
    assets
        .iter()
        .map(|asset| (item(&set.id, asset), Desired::Value(None)))
        .collect()
}

/// What a merge's publication adds to the library change it records, when the request asked for
/// its result in the development set (Pick merged, `add_to_development_set: true`): the result's
/// `set-member` item in the set `capture` names, after its members, or, when `capture` names none,
/// in a new one-result set created now. Refused with `conflict`, naming both sets, when the current
/// set is not the captured one — the person replaced it, or opened one, while the merge ran — so
/// the publication's transaction writes nothing. A result already a member keeps its place.
///
/// The merge's own undo removes exactly this member, and leaves every other member and every pick
/// as it is. Call it from `EditorService::publish_merge`'s `extra`, or, for an existing result Pick
/// merged reuses, as the items of its own library change.
pub(crate) fn membership_for_publication(
    tx: &Transaction<'_>,
    capture: &SetCapture,
    result: &AssetId,
) -> Result<Vec<(LibraryItem, Desired)>, Error> {
    let now = current(tx)?;
    let set = match (&capture.set_id, now) {
        (Some(captured), Some(set)) if set.id == *captured => set,
        (Some(captured), now) => {
            return Err(Error::conflict(format!(
                "the development set {captured} was replaced while the merge ran; nothing was \
                 published"
            ))
            .with_data(json!({
                "set_id": captured,
                "current": now.as_ref().map(|set| &set.id),
            })));
        }
        (None, Some(set)) => {
            return Err(Error::conflict(format!(
                "development set {} was opened while the merge ran; nothing was published",
                set.id
            ))
            .with_data(json!({"set_id": null, "current": set.id})));
        }
        (None, None) => create(tx, now_ms())?,
    };
    additions(&set, std::slice::from_ref(result))
}

/// A `set-member` item.
pub(crate) fn item(set: &DevelopmentSetId, asset: &AssetId) -> LibraryItem {
    LibraryItem::SetMember {
        set_id: set.clone(),
        asset_id: asset.clone(),
    }
}

fn member_value(position: u64) -> Result<Value, Error> {
    serde_json::to_value(SetMemberValue { position })
        .map_err(|error| Error::internal(format!("cannot encode a set member: {error}")))
}

/// A member's place in `set`, or `None` when it is not one (or either is gone).
pub(crate) fn member(
    connection: &Connection,
    set: &DevelopmentSetId,
    asset: &AssetId,
) -> Result<Option<u64>, Error> {
    let position: Option<i64> = connection
        .prepare_cached(
            "SELECT m.position FROM development_set_members m
             JOIN development_sets s ON s.row = m.set_row
             JOIN assets a ON a.row_id = m.asset_row
             WHERE s.id = ?1 AND a.id = ?2",
        )?
        .query_row([set.as_str(), asset.as_str()], |row| row.get(0))
        .optional()?;
    Ok(position.map(|position| position as u64))
}

/// Write the `set-member` item `item`: `asset` at `position` in `set`, or out of it. Refused with
/// `conflict` naming the item when the set is no longer kept or the photograph no longer in the
/// catalog, as an undo of a change to them must be. Moves the set's revision on.
pub(crate) fn write_member(
    tx: &Transaction<'_>,
    item: &LibraryItem,
    set: &DevelopmentSetId,
    asset: &AssetId,
    position: Option<u64>,
) -> Result<(), Error> {
    let row: Option<i64> = tx
        .prepare_cached("SELECT row FROM development_sets WHERE id = ?1")?
        .query_row([set.as_str()], |row| row.get(0))
        .optional()?;
    let Some(row) = row else {
        return Err(Error::conflict(format!(
            "development set {set} is no longer kept: it was retired and nothing names it"
        ))
        .with_data(json!({"items": [item], "count": 1})));
    };
    match position {
        Some(position) => {
            let written = tx
                .prepare_cached(
                    "INSERT INTO development_set_members (set_row, asset_row, position)
                     SELECT ?1, row_id, ?3 FROM assets WHERE id = ?2
                     ON CONFLICT(set_row, asset_row) DO UPDATE SET position = excluded.position",
                )?
                .execute(params![row, asset.as_str(), position as i64])
                .map_err(|error| taken(item, error))?;
            if written == 0 {
                return Err(Error::conflict(format!(
                    "photograph {asset} is no longer in the catalog"
                ))
                .with_data(json!({"items": [item], "count": 1})));
            }
            tx.prepare_cached(
                "UPDATE development_sets SET next_position = max(next_position, ?2) WHERE row = ?1",
            )?
            .execute(params![row, position as i64 + 1])?;
        }
        None => {
            tx.prepare_cached(
                "DELETE FROM development_set_members
                 WHERE set_row = ?1 AND asset_row = (SELECT row_id FROM assets WHERE id = ?2)",
            )?
            .execute(params![row, asset.as_str()])?;
        }
    }
    let revision = bump(tx)?;
    tx.prepare_cached("UPDATE development_sets SET revision = ?2 WHERE row = ?1")?
        .execute(params![row, revision as i64])?;
    Ok(())
}

/// Another member already at that place: only a set written outside its places' order holds one.
fn taken(item: &LibraryItem, error: rusqlite::Error) -> Error {
    match &error {
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            Error::conflict(format!(
                "{} {}: its place is taken",
                item.kind(),
                item.key()
            ))
            .with_data(json!({"items": [item], "count": 1}))
        }
        _ => Error::from(error),
    }
}

/// Whether any of the photographs `ids` (a JSON array of asset identities) is a member of a set:
/// before their records are deleted (a send-back, Empty Removed), which takes their memberships
/// with them and so [`touch`]es the sets.
pub(crate) fn holds_any(connection: &Connection, ids: &str) -> Result<bool, Error> {
    Ok(connection
        .prepare_cached(
            "SELECT EXISTS(SELECT 1 FROM development_set_members m
                 JOIN assets a ON a.row_id = m.asset_row
                 WHERE a.id IN (SELECT value FROM json_each(?1)))",
        )?
        .query_row([ids], |row| row.get(0))?)
}

/// Move the revision on for a change to the sets' members made outside their items: photographs
/// whose records were deleted with their memberships.
pub(crate) fn touch(tx: &Transaction<'_>) -> Result<(), Error> {
    let revision = bump(tx)?;
    tx.prepare_cached("UPDATE development_sets SET revision = ?1 WHERE current = 1")?
        .execute([revision as i64])?;
    Ok(())
}

/// Every set `asset` is a member of, with its place: what removing a merge's result takes it out
/// of. One read of the members' photograph index.
pub(crate) fn memberships_of(
    connection: &Connection,
    asset: &AssetId,
) -> Result<Vec<(DevelopmentSetId, u64)>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT s.id, m.position FROM development_set_members m
         JOIN development_sets s ON s.row = m.set_row
         WHERE m.asset_row = (SELECT row_id FROM assets WHERE id = ?1)
         ORDER BY s.row",
    )?;
    let rows = statement.query_map([asset.as_str()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    rows.map(|row| {
        let (set, position) = row?;
        Ok((DevelopmentSetId::parse(set)?, position as u64))
    })
    .collect()
}

/// The memberships of `asset` that the change which last put it in Removed took away, with the
/// place each had: what putting a merge's result back restores, in sets still kept. Reads the
/// journal's item index and that one change's rows.
pub(crate) fn taken_by_removal(
    connection: &Connection,
    asset: &AssetId,
) -> Result<Vec<(DevelopmentSetId, u64)>, Error> {
    let change: Option<i64> = connection
        .prepare_cached(
            "SELECT max(change_seq) FROM library_change_rows
             WHERE item_kind = 'asset-removal' AND item_key = ?1 AND after_json IS NOT NULL",
        )?
        .query_row([asset.as_str()], |row| row.get(0))?;
    let Some(change) = change else {
        return Ok(Vec::new());
    };
    let suffix = format!("/{asset}");
    let mut statement = connection.prepare_cached(
        "SELECT r.item_key, r.before_json FROM library_change_rows r
         WHERE r.change_seq = ?1 AND r.item_kind = 'set-member' AND r.after_json IS NULL
             AND r.before_json IS NOT NULL
             AND substr(r.item_key, length(r.item_key) - ?3 + 1) = ?2
             AND EXISTS (SELECT 1 FROM development_sets s
                 WHERE s.id = substr(r.item_key, 1, length(r.item_key) - ?3))
         ORDER BY r.ordinal",
    )?;
    let rows = statement.query_map(params![change, suffix, suffix.len() as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut taken = Vec::new();
    for row in rows {
        let (key, before) = row?;
        let Some(set) = key.strip_suffix(&suffix) else {
            continue;
        };
        let value: SetMemberValue = crate::editor::decode("stored set member", before)?;
        taken.push((DevelopmentSetId::parse(set)?, value.position));
    }
    Ok(taken)
}

/// The value a restored member is written with.
pub(crate) fn value_at(position: u64) -> Result<Value, Error> {
    member_value(position)
}

/// How many members `set` has.
pub(crate) fn count(connection: &Connection, set_row: i64) -> Result<u32, Error> {
    let count: i64 = connection
        .prepare_cached("SELECT count(*) FROM development_set_members WHERE set_row = ?1")?
        .query_row([set_row], |row| row.get(0))?;
    Ok(count as u32)
}

/// Members `from..from + limit` of the current set, in order, with its identity, revision and
/// size. One range read of the members' order index, joined to their photographs and merges.
pub(crate) fn page(
    connection: &Connection,
    from: u32,
    limit: usize,
) -> Result<DevelopmentSetPage, Error> {
    let revision = revision(connection)?;
    let Some(set) = current(connection)? else {
        return Ok(DevelopmentSetPage {
            set_id: None,
            revision,
            count: 0,
            from,
            members: Vec::new(),
        });
    };
    let count = count(connection, set.row)?;
    let mut statement = connection.prepare_cached(
        "SELECT a.id, a.file_name, a.source_kind, a.removed_ms, g.id
         FROM development_set_members m
         JOIN assets a ON a.row_id = m.asset_row
         LEFT JOIN merges g ON g.result_asset_id = a.id
         WHERE m.set_row = ?1 ORDER BY m.position LIMIT ?2 OFFSET ?3",
    )?;
    let rows = statement.query_map(params![set.row, limit as i64, from as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    let mut members = Vec::new();
    for (offset, row) in rows.enumerate() {
        let (asset, file_name, kind, removed, merge) = row?;
        members.push(SetMember {
            index: from + offset as u32,
            asset_id: AssetId::parse(asset)?,
            file_name,
            kind: SourceTag::parse(&kind)
                .ok_or_else(|| Error::incompatible(format!("photograph: unknown kind {kind}")))?,
            removed: removed.is_some(),
            merge_id: merge.map(MergeId::parse).transpose()?,
        });
    }
    Ok(DevelopmentSetPage {
        set_id: Some(set.id),
        revision,
        count,
        from,
        members,
    })
}

/// The current set's members' rows: what views mark In Development set. One range read of the
/// set's members, bounded by [`MAX_SET_MEMBERS`].
pub(crate) fn current_rows(connection: &Connection) -> Result<HashSet<AssetRowId>, Error> {
    let mut statement = connection.prepare_cached(
        "SELECT m.asset_row FROM development_sets s
         JOIN development_set_members m ON m.set_row = s.row WHERE s.current = 1",
    )?;
    let found = statement.query_map([], |row| row.get::<_, i64>(0))?;
    found
        .map(|row| Ok(AssetRowId(row?)))
        .collect::<Result<_, Error>>()
}

/// Which of `rows` are members of the current set: one read of the set's members among them, a
/// window's photographs.
pub(crate) fn in_current(
    connection: &Connection,
    rows: &[AssetRowId],
) -> Result<HashSet<AssetRowId>, Error> {
    if rows.is_empty() {
        return Ok(HashSet::new());
    }
    let list = crate::browse::json_list(&rows.iter().map(|row| row.0).collect::<Vec<_>>())?;
    let mut statement = connection.prepare_cached(
        "SELECT m.asset_row FROM development_set_members m
         JOIN development_sets s ON s.row = m.set_row AND s.current = 1
         WHERE m.asset_row IN (SELECT value FROM json_each(?1))",
    )?;
    let found = statement.query_map([list], |row| row.get::<_, i64>(0))?;
    found
        .map(|row| Ok(AssetRowId(row?)))
        .collect::<Result<_, Error>>()
}

#[cfg(test)]
#[path = "development_set_tests.rs"]
mod tests;
