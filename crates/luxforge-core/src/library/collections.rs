//! Collections, smart collections and groups (`docs/design/catalog.md`, "The catalog"): a plain
//! collection holds any photographs a person adds, across folders; a smart collection stores a
//! browse query over photographs that may not refer to another smart collection; a group holds
//! collections, smart collections and groups and no photographs. Each change is one library change
//! of `collection` and `membership` items through the journal.
//!
//! Each function here checks what the design refuses and plans the items, reading the catalog; the
//! owner records the plan ([`Planned::apply`]) in one transaction. Names follow [`super::tree`]'s
//! rules, among all the collections of one group (or the top level), whatever their kind. What a
//! smart collection finds is the views lane's to evaluate; this module only keeps its query.
use super::{
    folders::{self, counted},
    journal::Desired,
    tree::{self, Planned, checked_name},
};
use crate::{
    AssetId, Error,
    catalog_types::{
        AssetRowId, Collection, CollectionId, CollectionKind, CollectionValue, Collections,
        LibraryItem, MAX_LIBRARY_BATCH, MembershipValue, ViewQuery, ViewSource,
    },
    editor::library_rows::{COLLECTION_COLUMNS, collection, read_collection},
};
use rusqlite::Connection;
use serde_json::Value;

const TABLE: &str = "collections";

/// The collection `id`, refused as unknown when the catalog has none.
pub(crate) fn find(connection: &Connection, id: &CollectionId) -> Result<Collection, Error> {
    collection(connection, id)?.ok_or_else(|| Error::validation(format!("unknown collection {id}")))
}

/// What a person calls a collection of this kind.
fn noun(kind: CollectionKind) -> &'static str {
    match kind {
        CollectionKind::Collection => "collection",
        CollectionKind::Smart => "smart collection",
        CollectionKind::Group => "group",
    }
}

/// `collection.create` and `collection.create-smart`: a new collection of `kind` named `name` in
/// the group `parent` (the top level when none), holding `query` exactly when it is smart. The
/// collection it makes and the change that makes it.
pub(crate) fn create(
    connection: &Connection,
    name: &str,
    kind: CollectionKind,
    parent: Option<&CollectionId>,
    query: Option<ViewQuery>,
    now_ms: i64,
) -> Result<(CollectionValue, Planned), Error> {
    let name = checked_name(name)?;
    match (kind, &query) {
        (CollectionKind::Smart, Some(query)) => check_query(connection, query)?,
        (CollectionKind::Smart, None) => {
            return Err(Error::validation(
                "a smart collection is made with collection.create-smart and its query",
            ));
        }
        (_, Some(_)) => {
            return Err(Error::internal("only a smart collection holds a query"));
        }
        (_, None) => {}
    }
    if let Some(parent) = parent {
        group(connection, parent)?;
    }
    free(connection, &name, parent, None)?;
    let created = CollectionValue {
        id: CollectionId::new(),
        name,
        parent_id: parent.cloned(),
        kind,
        query,
        created_ms: now_ms,
    };
    let label = format!("Created {} {}", noun(kind), created.name);
    let changes = vec![set(&created)?];
    Ok((created, Planned::labelled(changes, label)))
}

/// `collection.update-smart`: a smart collection's query replaced, checked as when it was made.
pub(crate) fn update_smart(
    connection: &Connection,
    id: &CollectionId,
    query: ViewQuery,
) -> Result<Planned, Error> {
    let before = find(connection, id)?;
    if before.kind != CollectionKind::Smart {
        return Err(Error::validation(format!(
            "{} is a {}, and only a smart collection has a query",
            before.name,
            noun(before.kind)
        )));
    }
    check_query(connection, &query)?;
    let label = format!("Changed the query of smart collection {}", before.name);
    let after = CollectionValue {
        query: Some(query),
        ..CollectionValue::from(before)
    };
    Ok(Planned::labelled(vec![set(&after)?], label))
}

/// `collection.rename`: a collection, smart collection or group under a new name among the same
/// siblings.
pub(crate) fn rename(
    connection: &Connection,
    id: &CollectionId,
    name: &str,
) -> Result<Planned, Error> {
    let name = checked_name(name)?;
    let before = find(connection, id)?;
    free(connection, &name, before.parent_id.as_ref(), Some(id))?;
    let label = format!("Renamed {} {} to {name}", noun(before.kind), before.name);
    let after = CollectionValue {
        name,
        ..CollectionValue::from(before)
    };
    Ok(Planned::labelled(vec![set(&after)?], label))
}

/// `collection.move`: into the group `parent`, or to the top level; never into itself or a group
/// inside it.
pub(crate) fn move_to(
    connection: &Connection,
    id: &CollectionId,
    parent: Option<&CollectionId>,
) -> Result<Planned, Error> {
    let moved = find(connection, id)?;
    let label = match parent {
        Some(parent_id) => {
            let parent = group(connection, parent_id)?;
            if tree::within(parent_id.as_str(), id.as_str(), |node| {
                tree::parent_in(connection, TABLE, node)
            })? {
                let what = noun(moved.kind);
                return Err(Error::validation(if parent_id == id {
                    format!("{what} {} cannot be moved into itself", moved.name)
                } else {
                    format!(
                        "{what} {} cannot be moved into {}, which is inside it",
                        moved.name, parent.name
                    )
                }));
            }
            format!(
                "Moved {} {} into {}",
                noun(moved.kind),
                moved.name,
                parent.name
            )
        }
        None => format!("Moved {} {} to the top level", noun(moved.kind), moved.name),
    };
    free(connection, &moved.name, parent, Some(id))?;
    let after = CollectionValue {
        parent_id: parent.cloned(),
        ..CollectionValue::from(moved)
    };
    Ok(Planned::labelled(vec![set(&after)?], label))
}

/// `collection.delete`: a plain collection with its memberships (each membership, then the
/// collection, so the undo makes the collection before its members), a smart collection, or an
/// empty group; a group that holds anything is `conflict`. A collection of more members than one
/// change covers is `resource-limit`.
pub(crate) fn delete(connection: &Connection, id: &CollectionId) -> Result<Planned, Error> {
    let deleted = find(connection, id)?;
    let mut changes = Vec::new();
    match deleted.kind {
        CollectionKind::Group => {
            let held = children(connection, Some(id))?.len();
            if held > 0 {
                return Err(Error::conflict(format!(
                    "group {} cannot be deleted: it holds {}",
                    deleted.name,
                    counted(held, "collection", "collections")
                )));
            }
        }
        CollectionKind::Collection => {
            let members = members_of(connection, id)?;
            if members.len() >= MAX_LIBRARY_BATCH {
                return Err(Error::resource_limit(format!(
                    "collection {} has {} photographs, more than a library change covers with it ({MAX_LIBRARY_BATCH} items)",
                    deleted.name,
                    members.len()
                )));
            }
            changes.extend(members.into_iter().map(|asset_id| {
                (
                    LibraryItem::Membership {
                        collection_id: id.clone(),
                        asset_id,
                    },
                    Desired::Value(None),
                )
            }));
        }
        CollectionKind::Smart => {}
    }
    changes.push((
        LibraryItem::Collection {
            collection_id: id.clone(),
        },
        Desired::Value(None),
    ));
    let label = format!("Deleted {} {}", noun(deleted.kind), deleted.name);
    Ok(Planned::labelled(changes, label))
}

/// `collection.add` (`add`) and `collection.remove`: the photographs made members of a plain
/// collection since `now_ms`, a member keeping when it joined, or taken out of it. The label names
/// what changed and the collection's path through its groups ("Added 5 to Portfolio ›
/// Landscapes", "Removed DSC_0412.NEF from Portfolio").
pub(crate) fn members(
    connection: &Connection,
    id: &CollectionId,
    assets: Vec<(AssetId, AssetRowId)>,
    add: bool,
    now_ms: i64,
) -> Result<Planned, Error> {
    let target = find(connection, id)?;
    match target.kind {
        CollectionKind::Collection => {}
        CollectionKind::Smart => {
            return Err(Error::validation(format!(
                "{} is a smart collection: its query decides what it holds",
                target.name
            )));
        }
        CollectionKind::Group => {
            return Err(Error::validation(format!(
                "{} is a group, which holds collections, not photographs",
                target.name
            )));
        }
    }
    let path = path(connection, &target)?;
    let joined = serde_json::to_value(MembershipValue { added_ms: now_ms })
        .map_err(|error| Error::internal(format!("cannot encode a membership: {error}")))?;
    let changes = assets
        .into_iter()
        .map(|(asset_id, _)| {
            let desired = if add {
                Desired::UnlessPresent(joined.clone())
            } else {
                Desired::Value(None)
            };
            (
                LibraryItem::Membership {
                    collection_id: id.clone(),
                    asset_id,
                },
                desired,
            )
        })
        .collect();
    Ok(Planned::counted(changes, move |connection, rows| {
        let what = tree::photographs(connection, rows, |count| count.to_string());
        if add {
            format!("Added {what} to {path}")
        } else {
            format!("Removed {what} from {path}")
        }
    }))
}

/// Refuse a query a smart collection cannot store: one over files (an event, a folder on disk or a
/// card) rather than photographs, one no view can answer, and one whose source names a smart
/// collection, a group (which holds no photographs), or a collection or catalog folder the catalog
/// does not have.
pub(crate) fn check_query(connection: &Connection, query: &ViewQuery) -> Result<(), Error> {
    if query.source.over_files() {
        return Err(Error::validation(
            "a smart collection's query is over photographs, and an event, a folder on disk or a card lists files",
        ));
    }
    query.validate()?;
    match &query.source {
        ViewSource::Collection { collection_id } => {
            let named = find(connection, collection_id)?;
            match named.kind {
                CollectionKind::Collection => Ok(()),
                CollectionKind::Smart => Err(Error::validation(format!(
                    "a smart collection may not refer to another smart collection ({})",
                    named.name
                ))),
                CollectionKind::Group => Err(Error::validation(format!(
                    "{} is a group, which holds no photographs",
                    named.name
                ))),
            }
        }
        ViewSource::CatalogFolder { folder_id, .. } => {
            folders::folder(connection, folder_id).map(drop)
        }
        _ => Ok(()),
    }
}

/// `collection.list`: every collection, smart collection and group, parents before children and
/// siblings by name ignoring case, a plain collection with how many members it has (removed
/// photographs excepted). One query: the counts are one `GROUP BY` over the memberships.
pub(crate) fn list(connection: &Connection) -> Result<Collections, Error> {
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {COLLECTION_COLUMNS}, held.members
         FROM collections
         LEFT JOIN (
             SELECT m.collection_id AS collection, count(*) AS members
             FROM collection_members m JOIN assets a ON a.row_id = m.asset_row
             WHERE a.removed_ms IS NULL
             GROUP BY m.collection_id
         ) held ON held.collection = id"
    ))?;
    let collections = statement
        .query_map([], |row| {
            let mut collection = read_collection(row)?;
            let members: Option<i64> = row.get(6)?;
            collection.count = (collection.kind == CollectionKind::Collection)
                .then(|| members.unwrap_or(0).try_into().unwrap_or(u32::MAX));
            Ok(collection)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Collections {
        collections: tree::parents_first(
            collections,
            |collection| collection.id.as_str(),
            |collection| collection.parent_id.as_ref().map(CollectionId::as_str),
            |collection| &collection.name,
        ),
    })
}

/// A collection as an answer gives it: a plain collection counts its members, none when new.
pub(crate) fn answered(value: CollectionValue) -> Collection {
    let kind = value.kind;
    Collection {
        count: (kind == CollectionKind::Collection).then_some(0),
        ..Collection::from(value)
    }
}

/// The collection's path through its groups, as a person reads it: "Portfolio › Landscapes".
fn path(connection: &Connection, target: &Collection) -> Result<String, Error> {
    let mut names = vec![target.name.clone()];
    let mut seen = vec![target.id.clone()];
    let mut parent = target.parent_id.clone();
    while let Some(id) = parent.take() {
        if seen.contains(&id) {
            break;
        }
        let Some(group) = collection(connection, &id)? else {
            break;
        };
        names.push(group.name);
        parent = group.parent_id;
        seen.push(id);
    }
    names.reverse();
    Ok(names.join(" › "))
}

/// The group `id`, refused when it is unknown or not a group.
fn group(connection: &Connection, id: &CollectionId) -> Result<Collection, Error> {
    let group = find(connection, id)?;
    if group.kind != CollectionKind::Group {
        return Err(Error::validation(format!(
            "{} is a {}, and only a group holds collections",
            group.name,
            noun(group.kind)
        )));
    }
    Ok(group)
}

/// The item setting a collection to `value`.
fn set(value: &CollectionValue) -> Result<(LibraryItem, Desired), Error> {
    let encoded: Value = serde_json::to_value(value)
        .map_err(|error| Error::internal(format!("cannot encode a collection: {error}")))?;
    Ok((
        LibraryItem::Collection {
            collection_id: value.id.clone(),
        },
        Desired::Value(Some(encoded)),
    ))
}

/// The collections directly in `parent` (the top level when none): their identities and names.
fn children(
    connection: &Connection,
    parent: Option<&CollectionId>,
) -> Result<Vec<(String, String)>, Error> {
    Ok(connection
        .prepare_cached("SELECT id, name FROM collections WHERE parent_id IS ?1 ORDER BY id")?
        .query_map([parent.map(CollectionId::as_str)], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<_, _>>()?)
}

/// Refuse `name` in `parent` when a sibling other than `except` has it, ignoring case.
fn free(
    connection: &Connection,
    name: &str,
    parent: Option<&CollectionId>,
    except: Option<&CollectionId>,
) -> Result<(), Error> {
    let siblings = children(connection, parent)?;
    let Some((existing_id, existing)) =
        tree::clash(&siblings, name, except.map(CollectionId::as_str))
    else {
        return Ok(());
    };
    let kind = find(connection, &CollectionId::parse(existing_id)?)?.kind;
    let place = match parent {
        Some(parent) => format!("in {}", find(connection, parent)?.name),
        None => "at the top level".to_owned(),
    };
    Err(Error::conflict(format!(
        "there is already a {} named {existing} {place}",
        noun(kind)
    )))
}

/// Every member of a plain collection, removed photographs included, in the catalog's row order.
fn members_of(connection: &Connection, id: &CollectionId) -> Result<Vec<AssetId>, Error> {
    connection
        .prepare_cached(
            "SELECT a.id FROM collection_members m JOIN assets a ON a.row_id = m.asset_row
             WHERE m.collection_id = ?1 ORDER BY m.asset_row",
        )?
        .query_map([id.as_str()], |row| row.get::<_, String>(0))?
        .map(|id| AssetId::parse(id?))
        .collect()
}
