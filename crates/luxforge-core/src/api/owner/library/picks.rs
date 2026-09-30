//! `pick.set` and `pick.list` on the owner.
use super::{Call, Owner, change, retried, selected};
use crate::{
    Error,
    api::methods::value,
    catalog_types::{
        MAX_PICK_PAGE, PickPage, ViewSource,
        api::{PickList, PickSet},
    },
    editor::{now_ms, upsert_volume},
    library::{
        journal::{self, Request},
        picks::{self, PickScope},
        targets,
    },
};
use rusqlite::OptionalExtension;
use serde_json::Value;

/// `pick.set`: pick or clear the files `targets` names as one library change.
pub(in crate::api) fn pick_set(
    owner: &mut Owner,
    call: &Call<'_>,
    params: PickSet,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(answer);
    }
    let files = targets::files(&owner.service, &params.targets, || {
        selected(owner, call.client)
    })?;
    let planned = picks::changes(
        &owner.service.connection,
        files,
        params.picked,
        request,
        now_ms(),
    )?;
    let answer = change(owner, &call.origin, |tx| {
        for volume in &planned.volumes {
            upsert_volume(tx, volume)?;
        }
        journal::apply(tx, request, planned.changes, picks::label)
    })?;
    value(answer)
}

/// `pick.list`: the picks in path order after a cursor, of every file or of the files a source
/// holds, each with its index row when the index lists it.
pub(in crate::api) fn pick_list(
    owner: &mut Owner,
    _: &Call<'_>,
    params: PickList,
) -> Result<Value, Error> {
    let limit = params.limit.unwrap_or(1000).clamp(1, MAX_PICK_PAGE);
    let canonical;
    let scope = match &params.source {
        None => PickScope::All,
        Some(ViewSource::Folder { path, subfolders }) => {
            canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
            PickScope::Folder {
                path: &canonical,
                subfolders: *subfolders,
            }
        }
        Some(ViewSource::Card { volume_id }) => PickScope::Volume(volume_id),
        Some(ViewSource::Event { .. }) => {
            return Err(Error::validation(
                "pick.list cannot list an event's picks until events are evaluated",
            ));
        }
        Some(_) => {
            return Err(Error::validation(
                "pick.list's source lists files: an event, a folder or a card",
            ));
        }
    };
    let mut picks = picks::page(
        &owner.service.connection,
        scope,
        params.after.as_deref(),
        limit + 1,
    )?;
    let next_after = (picks.len() > limit).then(|| {
        picks.truncate(limit);
        picks.last().map(|pick| pick.path.clone())
    });
    if !picks.is_empty() {
        let index = owner.service.index()?;
        let mut row = index
            .connection()
            .prepare_cached("SELECT id FROM files WHERE path = ?1")?;
        for pick in &mut picks {
            pick.file_id = row
                .query_row([pick.path.to_string_lossy()], |found| found.get(0))
                .optional()?
                .map(crate::catalog_types::FileId);
        }
    }
    value(PickPage {
        picks,
        next_after: next_after.flatten(),
    })
}
