//! `library.journal`, `library.inspect`, `library.undo` and `library.redo` on the owner.
use super::{Call, Owner, change, retried};
use crate::{
    Error,
    api::methods::value,
    catalog_types::{
        MAX_JOURNAL_PAGE,
        api::{LibraryInspect, LibraryJournalParams, LibraryRequest},
    },
    library::journal::{self, Request},
};
use serde_json::Value;

/// `library.journal`: changes after a cursor, oldest first, without their rows.
pub(in crate::api) fn library_journal(
    owner: &mut Owner,
    _: &Call<'_>,
    params: LibraryJournalParams,
) -> Result<Value, Error> {
    let limit = params.limit.unwrap_or(100).clamp(1, MAX_JOURNAL_PAGE);
    value(journal::page(
        &owner.service.connection,
        params.after,
        limit,
    )?)
}

/// `library.inspect`: one change with each item's value before and after.
pub(in crate::api) fn library_inspect(
    owner: &mut Owner,
    _: &Call<'_>,
    params: LibraryInspect,
) -> Result<Value, Error> {
    value(journal::inspect(
        &owner.service.connection,
        params.sequence,
    )?)
}

/// `library.undo`: revert the calling actor's latest change not yet undone.
pub(in crate::api) fn library_undo(
    owner: &mut Owner,
    call: &Call<'_>,
    params: LibraryRequest,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(answer);
    }
    value(change(owner, &call.origin, |tx| {
        journal::undo(tx, request)
    })?)
}

/// `library.redo`: revert the calling actor's latest undo not yet redone.
pub(in crate::api) fn library_redo(
    owner: &mut Owner,
    call: &Call<'_>,
    params: LibraryRequest,
) -> Result<Value, Error> {
    let request = Request::new(&call.request.method, &params.mutation);
    if let Some(answer) = retried(owner, request)? {
        return value(answer);
    }
    value(change(owner, &call.origin, |tx| {
        journal::redo(tx, request)
    })?)
}
