//! Each library item's value, as the journal reads it before a change and writes it in the change,
//! its undo and its redo: one function that reads any item and one that writes any item, so the
//! journal applies a change and its inverse the same way whatever the change was. The values are
//! the shapes [`LibraryItem`] documents; `None` is an absent item (no pick, no folder, a photograph
//! not removed, not a member).
//!
//! A write validates only what the catalog holds: a photograph that is no longer in the catalog,
//! or a value the schema refuses (a sibling's name, a folder that is not empty, a file another
//! photograph names, a collection outside a group) is refused with `conflict` naming the item, and
//! the caller's transaction rolls back.
use crate::{
    AssetId, Error,
    catalog_types::{
        AssetFolderValue, AssetRemovalValue, AssetSourceValue, CatalogFolder, Collection,
        CollectionValue, DevelopedAssetValue, FolderValue, IndexedFolder, LibraryItem,
        MembershipValue, Pick,
    },
    editor::library_rows as rows,
};
use rusqlite::{Connection, ErrorCode, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The item's value now, or `None` when it is absent.
pub(crate) fn read(connection: &Connection, item: &LibraryItem) -> Result<Option<Value>, Error> {
    match item {
        LibraryItem::Pick { path } => rows::pick_at(connection, path)?.map(value).transpose(),
        LibraryItem::AssetFolder { asset_id } => rows::asset_folder(connection, asset_id)?
            .map(|folder_id| value(AssetFolderValue { folder_id }))
            .transpose(),
        LibraryItem::AssetRemoval { asset_id } => rows::asset_removed(connection, asset_id)?
            .flatten()
            .map(|removed_ms| value(AssetRemovalValue { removed_ms }))
            .transpose(),
        LibraryItem::AssetSource { asset_id } => rows::asset_source(connection, asset_id)?
            .map(value)
            .transpose(),
        LibraryItem::CatalogFolder { folder_id } => rows::catalog_folder(connection, folder_id)?
            .map(|folder| value(FolderValue::from(folder)))
            .transpose(),
        LibraryItem::Collection { collection_id } => rows::collection(connection, collection_id)?
            .map(|collection| value(CollectionValue::from(collection)))
            .transpose(),
        LibraryItem::Membership {
            collection_id,
            asset_id,
        } => rows::membership(connection, collection_id, asset_id)?
            .map(|added_ms| value(MembershipValue { added_ms }))
            .transpose(),
        LibraryItem::IndexedFolder { path } => rows::indexed_folder(connection, path)?
            .map(value)
            .transpose(),
        LibraryItem::DevelopedAsset { asset_id } => rows::asset_source(connection, asset_id)?
            .map(|source| {
                value(DevelopedAssetValue {
                    path: source.locator,
                })
            })
            .transpose(),
    }
}

/// Set the item to `to`, `None` removing it.
pub(crate) fn write(
    tx: &Transaction<'_>,
    item: &LibraryItem,
    to: Option<&Value>,
) -> Result<(), Error> {
    let sql = |result: rusqlite::Result<usize>| refused(item, result);
    match item {
        LibraryItem::Pick { path } => match to {
            None => sql(rows::delete_pick(tx, path)).map(drop),
            Some(to) => {
                let pick: Pick = typed(item, to)?;
                same(item, pick.path == *path && pick.file_id.is_none())?;
                sql(rows::put_pick(tx, &pick)).map(drop)
            }
        },
        LibraryItem::AssetFolder { asset_id } => {
            let AssetFolderValue { folder_id } = typed(item, required(item, to)?)?;
            present(
                asset_id,
                sql(rows::set_asset_folder(tx, asset_id, &folder_id))?,
            )
        }
        LibraryItem::AssetRemoval { asset_id } => {
            let removed_ms = to
                .map(|to| typed::<AssetRemovalValue>(item, to))
                .transpose()?
                .map(|removal| removal.removed_ms);
            present(
                asset_id,
                sql(rows::set_asset_removed(tx, asset_id, removed_ms))?,
            )
        }
        LibraryItem::AssetSource { asset_id } => {
            let source: AssetSourceValue = typed(item, required(item, to)?)?;
            present(
                asset_id,
                sql(rows::set_asset_source(tx, asset_id, &source))?,
            )
        }
        LibraryItem::CatalogFolder { folder_id } => match to {
            None => sql(rows::delete_catalog_folder(tx, folder_id)).map(drop),
            Some(to) => {
                let folder: FolderValue = typed(item, to)?;
                same(item, folder.id == *folder_id)?;
                sql(rows::put_catalog_folder(tx, &CatalogFolder::from(folder))).map(drop)
            }
        },
        LibraryItem::Collection { collection_id } => match to {
            None => sql(rows::delete_collection(tx, collection_id)).map(drop),
            Some(to) => {
                let collection: CollectionValue = typed(item, to)?;
                same(item, collection.id == *collection_id)?;
                sql(rows::put_collection(tx, &Collection::from(collection))).map(drop)
            }
        },
        LibraryItem::Membership {
            collection_id,
            asset_id,
        } => match to {
            None => sql(rows::delete_membership(tx, collection_id, asset_id)).map(drop),
            Some(to) => {
                let MembershipValue { added_ms } = typed(item, to)?;
                present(
                    asset_id,
                    sql(rows::put_membership(tx, collection_id, asset_id, added_ms))?,
                )
            }
        },
        LibraryItem::IndexedFolder { path } => match to {
            None => sql(rows::delete_indexed_folder(tx, path)).map(drop),
            Some(to) => {
                let folder: IndexedFolder = typed(item, to)?;
                same(item, folder.path == *path)?;
                sql(rows::put_indexed_folder(tx, &folder)).map(drop)
            }
        },
        // A Develop brings a photograph in by writing its asset, entry and capture rows, which a
        // value cannot carry, and its undo sends the photograph back: both are the develop lane's.
        LibraryItem::DevelopedAsset { asset_id } => Err(Error::conflict(format!(
            "photograph {asset_id} is brought in and sent back only by Develop"
        ))),
    }
}

/// A value as the journal stores it.
fn value(item: impl Serialize) -> Result<Value, Error> {
    serde_json::to_value(item)
        .map_err(|error| Error::internal(format!("cannot encode a library value: {error}")))
}

/// A stored value as its kind's type: a journal row this build cannot read is refused by name.
fn typed<'de, T: Deserialize<'de>>(item: &LibraryItem, value: &'de Value) -> Result<T, Error> {
    T::deserialize(value).map_err(|error| {
        Error::incompatible(format!(
            "a {} value this build cannot read: {error}",
            item.kind()
        ))
    })
}

/// The value of an item that is never absent while its photograph is in the catalog.
fn required<'a>(item: &LibraryItem, to: Option<&'a Value>) -> Result<&'a Value, Error> {
    to.ok_or_else(|| Error::internal(format!("a {} is never absent", item.kind())))
}

/// Refuse a value that names another item than its row.
fn same(item: &LibraryItem, matches: bool) -> Result<(), Error> {
    if matches {
        return Ok(());
    }
    Err(Error::internal(format!(
        "a {} value names another item than {}",
        item.kind(),
        item.key()
    )))
}

/// Refuse a write that found no photograph to change.
fn present(asset: &AssetId, changed: usize) -> Result<(), Error> {
    if changed == 0 {
        return Err(Error::conflict(format!(
            "photograph {asset} is no longer in the catalog"
        )));
    }
    Ok(())
}

/// A write's own result, a value the schema refused becoming a `conflict` naming the item: a valid
/// value fails only because the catalog around it has changed (a sibling took the name, the folder
/// is no longer empty, another photograph names the file).
fn refused(item: &LibraryItem, result: rusqlite::Result<usize>) -> Result<usize, Error> {
    result.map_err(|error| match &error {
        rusqlite::Error::SqliteFailure(failure, detail)
            if failure.code == ErrorCode::ConstraintViolation =>
        {
            Error::conflict(format!(
                "{} {}: {}",
                item.kind(),
                item.key(),
                detail
                    .as_deref()
                    .unwrap_or("the catalog refused the change")
            ))
        }
        _ => Error::from(error),
    })
}
