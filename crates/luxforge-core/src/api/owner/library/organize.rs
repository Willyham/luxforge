//! `folder.*`, `asset.move` and `collection.*` on the owner: catalog folders, moving photographs
//! between them, and collections, smart collections and groups (`crate::library::folders`,
//! `crate::library::collections`). Every change is one library change: a retry is answered first,
//! from the journal, then the change is planned against the catalog and recorded through
//! [`change`], which announces it as one event. Nothing on disk changes.
use super::{Call, Owner, change, retried, selected};
use crate::{
    Error, MutationRequest,
    api::{methods::value, params::NoParams},
    catalog_types::{
        CatalogFolder, CollectionAnswer, CollectionId, CollectionKind, CollectionValue,
        FolderAnswer, FolderValue, LibraryAnswer, LibraryChange, LibraryItem, ViewQuery,
        api::{
            AssetMove, CollectionCreate, CollectionCreateSmart, CollectionDelete,
            CollectionMembers, CollectionMove, CollectionRename, CollectionUpdateSmart,
            FolderCreate, FolderDelete, FolderMerge, FolderMove, FolderRename,
        },
    },
    editor::now_ms,
    library::{
        collections, folders,
        journal::{self, Outcome, Request},
        targets,
        tree::Planned,
    },
};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// `folder.list`.
pub(in crate::api) fn folder_list(
    owner: &mut Owner,
    _: &Call<'_>,
    _: NoParams,
) -> Result<Value, Error> {
    value(folders::list(&owner.service.connection)?)
}

/// `folder.create`: answers the change and the folder; a retry answers the folder its first
/// attempt made.
pub(in crate::api) fn folder_create(
    owner: &mut Owner,
    call: &Call<'_>,
    params: FolderCreate,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    let (folder, change): (FolderValue, _) = match first(owner, request)? {
        Some((change, folder)) => (folder, change),
        None => {
            let (folder, planned) = folders::create(
                &owner.service.connection,
                &params.name,
                params.parent_id.as_ref(),
                now_ms(),
            )?;
            (folder, record(owner, call, request, planned)?)
        }
    };
    value(FolderAnswer {
        deduplicated: change.deduplicated,
        change,
        folder: CatalogFolder::from(folder),
    })
}

/// `folder.rename`.
pub(in crate::api) fn folder_rename(
    owner: &mut Owner,
    call: &Call<'_>,
    params: FolderRename,
) -> Result<Value, Error> {
    planned_change(owner, call, &params.mutation, |owner| {
        folders::rename(&owner.service.connection, &params.folder_id, &params.name)
    })
}

/// `folder.move`.
pub(in crate::api) fn folder_move(
    owner: &mut Owner,
    call: &Call<'_>,
    params: FolderMove,
) -> Result<Value, Error> {
    planned_change(owner, call, &params.mutation, |owner| {
        folders::move_to(
            &owner.service.connection,
            &params.folder_id,
            params.parent_id.as_ref(),
        )
    })
}

/// `folder.merge`.
pub(in crate::api) fn folder_merge(
    owner: &mut Owner,
    call: &Call<'_>,
    params: FolderMerge,
) -> Result<Value, Error> {
    planned_change(owner, call, &params.mutation, |owner| {
        folders::merge(
            &owner.service.connection,
            &params.folder_id,
            &params.into_id,
        )
    })
}

/// `folder.delete`.
pub(in crate::api) fn folder_delete(
    owner: &mut Owner,
    call: &Call<'_>,
    params: FolderDelete,
) -> Result<Value, Error> {
    planned_change(owner, call, &params.mutation, |owner| {
        folders::delete(&owner.service.connection, &params.folder_id)
    })
}

/// `asset.move`.
pub(in crate::api) fn asset_move(
    owner: &mut Owner,
    call: &Call<'_>,
    params: AssetMove,
) -> Result<Value, Error> {
    planned_change(owner, call, &params.mutation, |owner| {
        let assets = targets::assets(&owner.service, &params.targets, || {
            selected(owner, call.client)
        })?;
        folders::move_assets(&owner.service.connection, assets, &params.folder_id)
    })
}

/// `collection.list`.
pub(in crate::api) fn collection_list(
    owner: &mut Owner,
    _: &Call<'_>,
    _: NoParams,
) -> Result<Value, Error> {
    value(collections::list(&owner.service.connection)?)
}

/// `collection.create`: a plain collection or a group.
pub(in crate::api) fn collection_create(
    owner: &mut Owner,
    call: &Call<'_>,
    params: CollectionCreate,
) -> Result<Value, Error> {
    create_collection(
        owner,
        call,
        &params.mutation,
        &params.name,
        params.kind,
        params.parent_id.as_ref(),
        None,
    )
}

/// `collection.create-smart`.
pub(in crate::api) fn collection_create_smart(
    owner: &mut Owner,
    call: &Call<'_>,
    params: CollectionCreateSmart,
) -> Result<Value, Error> {
    create_collection(
        owner,
        call,
        &params.mutation,
        &params.name,
        CollectionKind::Smart,
        params.parent_id.as_ref(),
        Some(params.query),
    )
}

/// `collection.update-smart`.
pub(in crate::api) fn collection_update_smart(
    owner: &mut Owner,
    call: &Call<'_>,
    params: CollectionUpdateSmart,
) -> Result<Value, Error> {
    let CollectionUpdateSmart {
        collection_id,
        query,
        mutation,
    } = params;
    planned_change(owner, call, &mutation, |owner| {
        collections::update_smart(&owner.service.connection, &collection_id, query)
    })
}

/// `collection.rename`.
pub(in crate::api) fn collection_rename(
    owner: &mut Owner,
    call: &Call<'_>,
    params: CollectionRename,
) -> Result<Value, Error> {
    planned_change(owner, call, &params.mutation, |owner| {
        collections::rename(
            &owner.service.connection,
            &params.collection_id,
            &params.name,
        )
    })
}

/// `collection.move`.
pub(in crate::api) fn collection_move(
    owner: &mut Owner,
    call: &Call<'_>,
    params: CollectionMove,
) -> Result<Value, Error> {
    planned_change(owner, call, &params.mutation, |owner| {
        collections::move_to(
            &owner.service.connection,
            &params.collection_id,
            params.parent_id.as_ref(),
        )
    })
}

/// `collection.delete`.
pub(in crate::api) fn collection_delete(
    owner: &mut Owner,
    call: &Call<'_>,
    params: CollectionDelete,
) -> Result<Value, Error> {
    planned_change(owner, call, &params.mutation, |owner| {
        collections::delete(&owner.service.connection, &params.collection_id)
    })
}

/// `collection.add`.
pub(in crate::api) fn collection_add(
    owner: &mut Owner,
    call: &Call<'_>,
    params: CollectionMembers,
) -> Result<Value, Error> {
    members(owner, call, params, true)
}

/// `collection.remove`.
pub(in crate::api) fn collection_remove(
    owner: &mut Owner,
    call: &Call<'_>,
    params: CollectionMembers,
) -> Result<Value, Error> {
    members(owner, call, params, false)
}

fn members(
    owner: &mut Owner,
    call: &Call<'_>,
    params: CollectionMembers,
    add: bool,
) -> Result<Value, Error> {
    planned_change(owner, call, &params.mutation, |owner| {
        let assets = targets::assets(&owner.service, &params.targets, || {
            selected(owner, call.client)
        })?;
        collections::members(
            &owner.service.connection,
            &params.collection_id,
            assets,
            add,
            now_ms(),
        )
    })
}

/// A collection, smart collection or group made: answers the change and the collection; a retry
/// answers the collection its first attempt made.
fn create_collection(
    owner: &mut Owner,
    call: &Call<'_>,
    mutation: &MutationRequest,
    name: &str,
    kind: CollectionKind,
    parent: Option<&CollectionId>,
    query: Option<ViewQuery>,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, mutation);
    let (collection, change): (CollectionValue, _) = match first(owner, request)? {
        Some((change, collection)) => (collection, change),
        None => {
            let (collection, planned) = collections::create(
                &owner.service.connection,
                name,
                kind,
                parent,
                query,
                now_ms(),
            )?;
            (collection, record(owner, call, request, planned)?)
        }
    };
    value(CollectionAnswer {
        deduplicated: change.deduplicated,
        change,
        collection: collections::answered(collection),
    })
}

/// A method that answers the library change it records: a retry is answered from the journal
/// before anything is planned or its targets resolved again; otherwise `plan` says what the change
/// is, and it is recorded.
fn planned_change(
    owner: &mut Owner,
    call: &Call<'_>,
    mutation: &MutationRequest,
    plan: impl FnOnce(&Owner) -> Result<Planned, Error>,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(answer);
    }
    let planned = plan(owner)?;
    value(record(owner, call, request, planned)?)
}

/// Record a planned change through the owner, which announces it.
fn record(
    owner: &mut Owner,
    call: &Call<'_>,
    request: Request<'_>,
    planned: Planned,
) -> Result<LibraryAnswer, Error> {
    change(owner, &call.origin, |tx| planned.apply(tx, request))
}

/// A create's first attempt, when this request is a retry of one: its answer and the value it made
/// (the `after` of its one row), read back from the journal, after a restart too.
fn first<T: DeserializeOwned>(
    owner: &Owner,
    request: Request<'_>,
) -> Result<Option<(LibraryAnswer, T)>, Error> {
    let Some(change) = journal::find(&owner.service.connection, request)? else {
        return Ok(None);
    };
    let made = made(owner, &change)?;
    Ok(Some((Outcome::deduplicated(change).answer(), made)))
}

fn made<T: DeserializeOwned>(owner: &Owner, change: &LibraryChange) -> Result<T, Error> {
    let detail = journal::inspect(&owner.service.connection, change.sequence.0)?;
    let after = detail
        .rows
        .into_iter()
        .find(|row| {
            matches!(
                row.item,
                LibraryItem::CatalogFolder { .. } | LibraryItem::Collection { .. }
            )
        })
        .and_then(|row| row.after)
        .ok_or_else(|| {
            Error::internal(format!(
                "library change {} made nothing to answer",
                change.sequence.0
            ))
        })?;
    serde_json::from_value(after).map_err(|error| {
        Error::incompatible(format!(
            "library change {} holds a value this build cannot read: {error}",
            change.sequence.0
        ))
    })
}
