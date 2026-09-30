//! **Lane C (catalog)** on the owner: the develop lane's handle (its worker, started on the first
//! Develop), the source search and batch jobs, what they post back (a committed batch, a finished
//! file, progress), and the handlers of `pick.*`, `folder.*`, `asset.move`, `asset.send-back`,
//! `asset.remove`, `asset.restore`, `collection.*`, `library.*`, `catalog.empty-removed`,
//! `source.check`, `source.missing`, `source.find`, `source.locate`, `source.relink`, `batch.*` and
//! `catalog.info` (`crate::catalog_types::api`). The library itself is `crate::library`.
#![allow(dead_code, reason = "catalog contracts: lane C fills this as it lands")]

use super::{ClientId, Owner, catalog::Poster};
use crate::{JobId, activity::ActivityBoard};
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
