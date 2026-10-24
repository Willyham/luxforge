//! `asset.remove`, `asset.restore` and `catalog.empty-removed` on the owner
//! (`crate::library::remove`). Removing and putting back are library changes: a retry is answered
//! first, from the journal, then the photographs `targets` names are resolved and the change is
//! recorded through [`change`], which announces it as one event. Emptying Removed needs permission
//! authority (P12) and is not a library change: its first answer is kept by the owner's request
//! table, which answers a retry.
use super::{super::SourceTaskKind, Call, Owner, change, retried, selected};
use crate::{
    AssetId, Error, MutationOutcome,
    api::{announce_once, methods::value},
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

/// `asset.remove`: move the photographs `targets` names to Removed as one library change, a merge's
/// result with its stack deactivated. Their retained sample grids are released with it.
pub(in crate::api) fn asset_remove(
    owner: &mut Owner,
    call: &Call<'_>,
    params: AssetTargets,
) -> Result<Value, Error> {
    let context = owner.service.render_context().clone();
    targeted(owner, call, &params, |connection, assets| {
        for (asset, _) in &assets {
            context.release_grids(asset);
        }
        remove::remove(connection, assets, now_ms())
    })
}

/// `asset.restore`: put the photographs `targets` names back, as one library change, a merge's
/// result with its stack active again.
pub(in crate::api) fn asset_restore(
    owner: &mut Owner,
    call: &Call<'_>,
    params: AssetTargets,
) -> Result<Value, Error> {
    targeted(owner, call, &params, remove::restore)
}

/// Answer a retry from the journal, or resolve `targets` to photographs, plan the change and
/// record it.
fn targeted(
    owner: &mut Owner,
    call: &Call<'_>,
    params: &AssetTargets,
    plan: impl FnOnce(&rusqlite::Connection, Vec<(AssetId, AssetRowId)>) -> Result<Planned, Error>,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(answer);
    }
    let assets = targets::assets(&owner.service, &params.targets, || {
        selected(owner, call.client)
    })?;
    let planned = plan(&owner.service.connection, assets)?;
    value(change(owner, &call.origin, |tx| {
        planned.apply(tx, request)
    })?)
}

/// `catalog.empty-removed`, which only a client with permission authority reaches (the method table
/// declares it and the catalog owner refuses anyone else): delete the removed photographs' records, at most [`MAX_LIBRARY_BATCH`] of them, in
/// one transaction ([`EditorService::empty_removed`](crate::EditorService::empty_removed)),
/// announce it as one event, and queue the collection that removes the files of the artifacts it
/// deleted, as `artifact.collect` queues one: should the source queue be full, those files are
/// orphans the next collection removes.
pub(in crate::api) fn catalog_empty_removed(
    owner: &mut Owner,
    call: &Call<'_>,
    _: LibraryRequest,
) -> Result<Value, Error> {
    let (emptied, collection) = owner.service.empty_removed(MAX_LIBRARY_BATCH)?;
    let Emptied {
        assets,
        merges,
        remaining,
        ..
    } = &emptied;
    if !assets.is_empty() {
        announce_once(&mut owner.announced, &call.origin);
        // Every view is stale, though no library change or index revision moved; and their
        // rendered previews go too, now that the deletion has committed.
        super::super::views::photographs_deleted(owner);
        super::super::previews::forget_photographs(owner, assets);
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
        merges: u32::try_from(*merges).unwrap_or(u32::MAX),
        remaining: u32::try_from(*remaining).unwrap_or(u32::MAX),
        deduplicated: false,
    })
}
