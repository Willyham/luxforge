//! **Lane C (catalog)** on the owner: the develop lane's handle (its worker, started on the first
//! Develop), the source search and batch jobs, what they post back (a committed batch, a finished
//! file, progress), and the handlers of `pick.*`, `folder.*`, `asset.move`, `asset.send-back`,
//! `asset.remove`, `asset.restore`, `collection.*`, `library.*`, `catalog.empty-removed`,
//! `source.check`, `source.missing`, `source.find`, `source.locate`, `source.relink`, `batch.*` and
//! `catalog.info` (`crate::catalog_types::api`). The library itself is `crate::library`.
//!
//! One file per family of methods beside this one: `picks.rs` (`pick.*`), `journal.rs`
//! (`library.*`) and `info.rs` (`catalog.info`). Every method that changes the library records it
//! through [`change`], which runs the journal in one catalog transaction and announces the change
//! it recorded as one event.
#![allow(dead_code, reason = "catalog contracts: lane C fills this as it lands")]

mod info;
mod journal;
#[cfg(test)]
mod journal_tests;
mod picks;

pub(in crate::api) use info::catalog_info;
pub(in crate::api) use journal::{library_inspect, library_journal, library_redo, library_undo};
pub(in crate::api) use picks::{pick_list, pick_set};

use super::{Call, ClientId, Owner, catalog::Poster};
use crate::{
    Error, JobId,
    activity::ActivityBoard,
    api::announce_once,
    catalog_types::{LibraryAnswer, ViewItem},
    library::journal::{self as library_journal, Outcome, Request},
};
use rusqlite::Transaction;
use std::sync::Arc;

/// Lane C's state on the owner.
pub(super) struct LibraryLane {}

/// What lane C's workers post back.
pub(super) enum LibraryMessage {}

impl LibraryLane {
    pub(super) fn new(_: Poster, _: Arc<ActivityBoard>) -> Self {
        Self {}
    }

    pub(super) fn disconnect(&mut self, _: ClientId) {}

    /// `job.cancel` cancelled one of this lane's jobs in the job table.
    pub(super) fn cancelled(&mut self, _: &JobId) {}

    pub(super) fn shutdown(self) {}
}

pub(super) fn handle(_: &mut Owner, message: LibraryMessage) {
    match message {}
}

/// Record one library change and announce it: `change` runs the journal in one catalog
/// transaction ([`EditorService::library_write`](crate::EditorService::library_write)), and a
/// change it recorded now is one event naming its sequence, however many items it covered. A retry
/// the journal answered announces nothing, as its first attempt did.
///
/// Every lane's library change goes through here, lane A's indexed folders included: its
/// `index.add-folder` passes `|tx| { upsert_volume(tx, &volume)?; journal::apply(tx, request,
/// vec![(LibraryItem::IndexedFolder { path }, Desired::Value(Some(folder)))], label) }`, and
/// `index.remove-folder` the same with `Desired::Value(None)`; undo and redo rewrite the
/// `indexed_folders` row from the journal like any other item.
pub(super) fn change(
    owner: &mut Owner,
    origin: &crate::api::Origin,
    change: impl FnOnce(&Transaction<'_>) -> Result<Outcome, Error>,
) -> Result<LibraryAnswer, Error> {
    let outcome = owner.service.library_write(change)?;
    if let Some(sequence) = outcome.announced() {
        announce_once(&mut owner.announced, &origin.clone().library(sequence));
    }
    Ok(outcome.answer())
}

/// The answer a request's first attempt recorded, when it recorded a change: a retry is answered
/// before its targets are resolved again, so it neither reads the disk nor fails on a file that has
/// moved since.
pub(super) fn retried(owner: &Owner, request: Request<'_>) -> Result<Option<LibraryAnswer>, Error> {
    Ok(
        library_journal::find(&owner.service.connection, request)?.map(|change| {
            Outcome::Recorded {
                change,
                sources: Vec::new(),
                deduplicated: true,
            }
            .answer()
        }),
    )
}

/// The items selected in the calling client's view.
fn selected(owner: &Owner, client: ClientId) -> Result<Vec<ViewItem>, Error> {
    owner.catalog.views.selected(client)
}
