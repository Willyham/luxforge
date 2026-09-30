//! `asset.remove`, `asset.restore` and `catalog.empty-removed` on the owner
//! (`crate::library::remove`). Removing and putting back are library changes: a retry is answered
//! first, from the journal, then the photographs `targets` names are resolved and the change is
//! recorded through [`change`], which announces it as one event. Emptying Removed needs permission
//! authority (P12) and is not a library change: its first answer is kept by the owner's request
//! table, which answers a retry.
use super::{super::SourceTaskKind, Call, Owner, change, retried, selected};
use crate::{
    AssetId, Error, MutationOutcome,
    api::{ClientAuthority, announce_once, methods::value},
    catalog_types::{
        AssetRowId, EmptyRemovedAnswer, MAX_LIBRARY_BATCH,
        api::{AssetTargets, LibraryRequest},
    },
    editor::now_ms,
    library::{
        journal::Request,
        remove::{self, Emptied},
        targets,
        tree::Planned,
    },
};
use serde_json::Value;

/// `asset.remove`: move the photographs `targets` names to Removed as one library change.
pub(in crate::api) fn asset_remove(
    owner: &mut Owner,
    call: &Call<'_>,
    params: AssetTargets,
) -> Result<Value, Error> {
    targeted(owner, call, &params, |assets| {
        remove::remove(assets, now_ms())
    })
}

/// `asset.restore`: put the photographs `targets` names back, as one library change.
pub(in crate::api) fn asset_restore(
    owner: &mut Owner,
    call: &Call<'_>,
    params: AssetTargets,
) -> Result<Value, Error> {
    targeted(owner, call, &params, |assets| Ok(remove::restore(assets)))
}

/// Answer a retry from the journal, or resolve `targets` to photographs, plan the change and
/// record it.
fn targeted(
    owner: &mut Owner,
    call: &Call<'_>,
    params: &AssetTargets,
    plan: impl FnOnce(Vec<(AssetId, AssetRowId)>) -> Result<Planned, Error>,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(answer);
    }
    let assets = targets::assets(&owner.service, &params.targets, || {
        selected(owner, call.client)
    })?;
    let planned = plan(assets)?;
    value(change(owner, &call.origin, |tx| {
        planned.apply(tx, request)
    })?)
}

/// `catalog.empty-removed`: refused with `forbidden` to a client without permission authority;
/// otherwise delete the removed photographs' records, at most [`MAX_LIBRARY_BATCH`] of them, in
/// one transaction ([`EditorService::empty_removed`](crate::EditorService::empty_removed)),
/// announce it as one event, and queue the collection that removes the files of the artifacts it
/// deleted, as `artifact.collect` queues one: should the source queue be full, those files are
/// orphans the next collection removes.
pub(in crate::api) fn catalog_empty_removed(
    owner: &mut Owner,
    call: &Call<'_>,
    _: LibraryRequest,
) -> Result<Value, Error> {
    if owner.authority(call.client) != ClientAuthority::Permissions {
        return Err(Error::forbidden(
            "emptying Removed permanently deletes photographs' history and needs permission \
             authority: only the desktop or luxforge-json --permission-authority may",
        ));
    }
    let (emptied, collection) = owner.service.empty_removed(MAX_LIBRARY_BATCH)?;
    let Emptied {
        assets, remaining, ..
    } = &emptied;
    if !assets.is_empty() {
        announce_once(&mut owner.announced, &call.origin);
    }
    if let Some(collection) = collection {
        let root = collection.root.clone();
        let _ = owner.sources.enqueue_maintenance(
            &mut owner.jobs,
            call.client,
            root,
            SourceTaskKind::Collect(collection),
        );
    }
    value(EmptyRemovedAnswer {
        outcome: if assets.is_empty() {
            MutationOutcome::NoOp
        } else {
            MutationOutcome::Applied
        },
        deleted: u32::try_from(assets.len()).unwrap_or(u32::MAX),
        remaining: u32::try_from(*remaining).unwrap_or(u32::MAX),
        deduplicated: false,
    })
}
