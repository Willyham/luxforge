//! Catalog folders and moving photographs between them (`docs/design/catalog.md`, "The catalog",
//! P15): every developed photograph is in exactly one catalog folder, and folders are created,
//! renamed, nested, merged and deleted when empty, each as one library change of `catalog-folder`
//! and `asset-folder` items through the journal. A catalog folder is the catalog's own organization:
//! nothing on disk changes when one does.
//!
//! Each function here checks what the design refuses and plans the items, reading the catalog; the
//! owner records the plan ([`Planned::apply`]) in one transaction. The rules a folder's name follows
//! are [`super::tree`]'s.
use super::{
    journal::Desired,
    tree::{self, Planned, checked_name},
};
use crate::{
    AssetId, Error,
    catalog_types::{
        AssetFolderValue, AssetRowId, CatalogFolder, CatalogFolderId, CatalogFolders, FolderValue,
        LibraryItem, MAX_LIBRARY_BATCH,
    },
    editor::library_rows::{CATALOG_FOLDER_COLUMNS, catalog_folder, read_catalog_folder},
};
use rusqlite::Connection;
use serde_json::Value;

const TABLE: &str = "catalog_folders";

/// The folder `id`, refused as unknown when the catalog has none.
pub(crate) fn folder(
    connection: &Connection,
    id: &CatalogFolderId,
) -> Result<CatalogFolder, Error> {
    catalog_folder(connection, id)?
        .ok_or_else(|| Error::validation(format!("unknown catalog folder {id}")))
}

/// `folder.create`: a new, empty folder named `name` in `parent` (the top level when none). The
/// folder it makes and the change that makes it.
pub(crate) fn create(
    connection: &Connection,
    name: &str,
    parent: Option<&CatalogFolderId>,
    now_ms: i64,
) -> Result<(FolderValue, Planned), Error> {
    let name = checked_name(name)?;
    if let Some(parent) = parent {
        folder(connection, parent)?;
    }
    free(connection, &name, parent, None)?;
    let created = FolderValue {
        id: CatalogFolderId::new(),
        name,
        parent_id: parent.cloned(),
        created_ms: now_ms,
        event: None,
    };
    let label = format!("Created folder {}", created.name);
    let changes = vec![set(&created)?];
    Ok((created, Planned::labelled(changes, label)))
}

/// `folder.rename`: the folder under a new name among the same siblings.
pub(crate) fn rename(
    connection: &Connection,
    id: &CatalogFolderId,
    name: &str,
) -> Result<Planned, Error> {
    let name = checked_name(name)?;
    let before = folder(connection, id)?;
    free(connection, &name, before.parent_id.as_ref(), Some(id))?;
    let label = format!("Renamed folder {} to {name}", before.name);
    let after = FolderValue {
        name,
        ..FolderValue::from(before)
    };
    Ok(Planned::labelled(vec![set(&after)?], label))
}

/// `folder.move`: the folder nested in `parent`, or at the top level; never in itself or a folder
/// inside it.
pub(crate) fn move_to(
    connection: &Connection,
    id: &CatalogFolderId,
    parent: Option<&CatalogFolderId>,
) -> Result<Planned, Error> {
    let moved = folder(connection, id)?;
    let label = match parent {
        Some(parent_id) => {
            let parent = folder(connection, parent_id)?;
            if inside(connection, parent_id, id)? {
                return Err(Error::validation(if parent_id == id {
                    format!("folder {} cannot be moved into itself", moved.name)
                } else {
                    format!(
                        "folder {} cannot be moved into {}, which is inside it",
                        moved.name, parent.name
                    )
                }));
            }
            format!("Moved folder {} into {}", moved.name, parent.name)
        }
        None => format!("Moved folder {} to the top level", moved.name),
    };
    free(connection, &moved.name, parent, Some(id))?;
    let after = FolderValue {
        parent_id: parent.cloned(),
        ..FolderValue::from(moved)
    };
    Ok(Planned::labelled(vec![set(&after)?], label))
}

/// `folder.merge`: every photograph in the folder, removed ones too, moves into `into`, then its
/// subfolders move under `into`, then `into` takes the folder's event span when it has none, then the
/// folder is deleted, as one change whose reverse (the folder again, `into`'s span as it was, its
/// subfolders back, its photographs back) is its undo. A subfolder whose name one of
/// `into`'s folders already has is refused naming it; a merge into itself or a folder inside it is
/// refused; more than [`MAX_LIBRARY_BATCH`] items is `resource-limit`.
pub(crate) fn merge(
    connection: &Connection,
    id: &CatalogFolderId,
    into: &CatalogFolderId,
) -> Result<Planned, Error> {
    let merged = folder(connection, id)?;
    let target = folder(connection, into)?;
    if inside(connection, into, id)? {
        return Err(Error::validation(if into == id {
            format!("folder {} cannot be merged into itself", merged.name)
        } else {
            format!(
                "folder {} cannot be merged into {}, which is inside it",
                merged.name, target.name
            )
        }));
    }
    let (photographs, _) = held(connection, id)?;
    let subfolders = children(connection, Some(id))?;
    // The receiving folder takes the merged folder's event span when it has none, so later picks
    // from that event still propose it.
    let carried = merged.event.clone().filter(|_| target.event.is_none());
    let items = photographs as usize + subfolders.len() + 1 + usize::from(carried.is_some());
    if items > MAX_LIBRARY_BATCH {
        return Err(Error::resource_limit(format!(
            "merging {} changes {items} items, more than the {MAX_LIBRARY_BATCH} a library change covers",
            merged.name
        )));
    }
    let receiving = children(connection, Some(into))?;
    for (sub_id, sub_name) in &subfolders {
        if let Some((_, existing)) = tree::clash(&receiving, sub_name, Some(sub_id)) {
            return Err(Error::conflict(format!(
                "folder {} cannot be merged into {}: {} already holds a folder named {existing}",
                merged.name, target.name, target.name
            )));
        }
    }
    let mut changes = Vec::with_capacity(items);
    let moved_to = serde_value(AssetFolderValue {
        folder_id: into.clone(),
    })?;
    for asset_id in assets_in(connection, id)? {
        changes.push((
            LibraryItem::AssetFolder { asset_id },
            Desired::Value(Some(moved_to.clone())),
        ));
    }
    for (sub_id, _) in subfolders {
        let sub = folder(connection, &CatalogFolderId::parse(sub_id)?)?;
        changes.push(set(&FolderValue {
            parent_id: Some(into.clone()),
            ..FolderValue::from(sub)
        })?);
    }
    let label = format!("Merged {} into {}", merged.name, target.name);
    if let Some(event) = carried {
        changes.push(set(&FolderValue {
            event: Some(event),
            ..FolderValue::from(target)
        })?);
    }
    changes.push((
        LibraryItem::CatalogFolder {
            folder_id: id.clone(),
        },
        Desired::Value(None),
    ));
    Ok(Planned::labelled(changes, label))
}

/// `folder.delete`: an empty folder, one that no photograph, removed or not, and no folder is in;
/// otherwise `conflict` saying what it holds.
pub(crate) fn delete(connection: &Connection, id: &CatalogFolderId) -> Result<Planned, Error> {
    let deleted = folder(connection, id)?;
    let (photographs, removed) = held(connection, id)?;
    let subfolders = children(connection, Some(id))?.len();
    let mut holds = Vec::new();
    if photographs > 0 {
        let mut said = counted(photographs as usize, "photograph", "photographs");
        if removed > 0 {
            said.push_str(&format!(
                " ({} in Removed)",
                if removed == photographs {
                    "all".to_owned()
                } else {
                    format!("{removed} of them")
                }
            ));
        }
        holds.push(said);
    }
    if subfolders > 0 {
        holds.push(counted(subfolders, "folder", "folders"));
    }
    if !holds.is_empty() {
        return Err(Error::conflict(format!(
            "folder {} cannot be deleted: it holds {}",
            deleted.name,
            holds.join(" and ")
        )));
    }
    let label = format!("Deleted folder {}", deleted.name);
    Ok(Planned::labelled(
        vec![(
            LibraryItem::CatalogFolder {
                folder_id: id.clone(),
            },
            Desired::Value(None),
        )],
        label,
    ))
}

/// `asset.move`: the photographs moved into `into`; the journal leaves out those already there, and
/// the label names what moved ("Moved DSC_0412.NEF to Konstanz", "Moved 5 photographs to
/// Konstanz").
pub(crate) fn move_assets(
    connection: &Connection,
    assets: Vec<(AssetId, AssetRowId)>,
    into: &CatalogFolderId,
) -> Result<Planned, Error> {
    let target = folder(connection, into)?;
    let moved_to = serde_value(AssetFolderValue {
        folder_id: into.clone(),
    })?;
    let changes = assets
        .into_iter()
        .map(|(asset_id, _)| {
            (
                LibraryItem::AssetFolder { asset_id },
                Desired::Value(Some(moved_to.clone())),
            )
        })
        .collect();
    Ok(Planned::counted(changes, move |connection, rows| {
        let what = tree::photographs(connection, rows, |count| {
            counted(count, "photograph", "photographs")
        });
        format!("Moved {what} to {}", target.name)
    }))
}

/// `folder.list`: every folder, parents before children and siblings by name ignoring case, each
/// with the photographs directly in it (removed ones excepted) and the capture year of its earliest
/// photograph (the camera-local day of the one with the earliest capture instant), none when it has
/// none dated. One query: the counts and years are one `GROUP BY` over the photographs.
pub(crate) fn list(connection: &Connection) -> Result<CatalogFolders, Error> {
    // With exactly one min() in the query, SQLite takes the bare `local_day` from the row holding
    // the minimum; when no photograph is dated both are null, as the capture table ties them.
    let mut statement = connection.prepare_cached(&format!(
        "SELECT {CATALOG_FOLDER_COLUMNS}, ifnull(held.photographs, 0),
             CAST(substr(held.first_day, 1, 4) AS INTEGER)
         FROM catalog_folders
         LEFT JOIN (
             SELECT a.catalog_folder_id AS folder, count(*) AS photographs,
                 min(c.capture_ms) AS first_ms, c.local_day AS first_day
             FROM assets a LEFT JOIN capture c ON c.asset_row = a.row_id
             WHERE a.removed_ms IS NULL
             GROUP BY a.catalog_folder_id
         ) held ON held.folder = id"
    ))?;
    let folders = statement
        .query_map([], |row| {
            let mut folder = read_catalog_folder(row)?;
            folder.count = row.get::<_, i64>(7)?.try_into().unwrap_or(u32::MAX);
            folder.year = row.get(8)?;
            Ok(folder)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(CatalogFolders {
        folders: tree::parents_first(
            folders,
            |folder| folder.id.as_str(),
            |folder| folder.parent_id.as_ref().map(CatalogFolderId::as_str),
            |folder| &folder.name,
        ),
    })
}

/// "1 photograph", "5 photographs".
pub(crate) fn counted(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// The item setting a folder to `value`.
fn set(value: &FolderValue) -> Result<(LibraryItem, Desired), Error> {
    Ok((
        LibraryItem::CatalogFolder {
            folder_id: value.id.clone(),
        },
        Desired::Value(Some(serde_value(value)?)),
    ))
}

fn serde_value(value: impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(value)
        .map_err(|error| Error::internal(format!("cannot encode a library value: {error}")))
}

/// Whether `node` is `ancestor` or inside it.
fn inside(
    connection: &Connection,
    node: &CatalogFolderId,
    ancestor: &CatalogFolderId,
) -> Result<bool, Error> {
    tree::within(node.as_str(), ancestor.as_str(), |id| {
        tree::parent_in(connection, TABLE, id)
    })
}

/// The folders directly in `parent` (the top level when none): their identities and names.
fn children(
    connection: &Connection,
    parent: Option<&CatalogFolderId>,
) -> Result<Vec<(String, String)>, Error> {
    Ok(connection
        .prepare_cached("SELECT id, name FROM catalog_folders WHERE parent_id IS ?1 ORDER BY id")?
        .query_map([parent.map(CatalogFolderId::as_str)], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?
        .collect::<Result<_, _>>()?)
}

/// Refuse `name` in `parent` when a sibling other than `except` has it, ignoring case.
fn free(
    connection: &Connection,
    name: &str,
    parent: Option<&CatalogFolderId>,
    except: Option<&CatalogFolderId>,
) -> Result<(), Error> {
    let siblings = children(connection, parent)?;
    let Some((_, existing)) = tree::clash(&siblings, name, except.map(CatalogFolderId::as_str))
    else {
        return Ok(());
    };
    let place = match parent {
        Some(parent) => format!("in {}", folder(connection, parent)?.name),
        None => "at the top level".to_owned(),
    };
    Err(Error::conflict(format!(
        "there is already a folder named {existing} {place}"
    )))
}

/// How many photographs name the folder, removed ones included, and how many of them are removed.
fn held(connection: &Connection, id: &CatalogFolderId) -> Result<(u64, u64), Error> {
    Ok(connection
        .prepare_cached(
            "SELECT count(*), count(removed_ms) FROM assets WHERE catalog_folder_id = ?1",
        )?
        .query_row([id.as_str()], |row| {
            Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64))
        })?)
}

/// Every photograph that names the folder, removed ones included, in the catalog's row order.
fn assets_in(connection: &Connection, id: &CatalogFolderId) -> Result<Vec<AssetId>, Error> {
    connection
        .prepare_cached("SELECT id FROM assets WHERE catalog_folder_id = ?1 ORDER BY row_id")?
        .query_map([id.as_str()], |row| row.get::<_, String>(0))?
        .map(|id| AssetId::parse(id?))
        .collect()
}
