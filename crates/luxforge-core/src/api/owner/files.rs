//! **Lane A (files)** on the owner: the index lane's handle (its two workers, the watchers and the
//! card monitor, started on first use), what they post back (a listing's progress and its
//! committed rows, a watched change, a mount), and the handlers of `index.add-folder`,
//! `index.remove-folder`, `index.folders`, `index.refresh` and `card.list`
//! (`crate::catalog_types::api`). The index lane itself is `crate::index`.
#![allow(dead_code, reason = "catalog contracts: lane A fills this as it lands")]

use super::{ClientId, Owner, catalog::Poster};
use crate::{JobId, activity::ActivityBoard};
use std::sync::Arc;

/// Lane A's state on the owner.
pub(super) struct FilesLane {}

/// What lane A's workers post back.
pub(super) enum FilesMessage {}

impl FilesLane {
    pub(super) fn new(_: Poster, _: Arc<ActivityBoard>) -> Self {
        Self {}
    }

    pub(super) fn disconnect(&mut self, _: ClientId) {}

    /// `job.cancel` cancelled one of this lane's jobs in the job table.
    pub(super) fn cancelled(&mut self, _: &JobId) {}

    /// A committed library change (an `index.add-folder` or `index.remove-folder`, or the undo or
    /// redo of one) changed whether these folders are indexed: `indexed_folders` holds the answer.
    /// Lane A owns the body; lane C calls it after every committed library change that touched one.
    pub(super) fn indexed_folders_changed(&mut self, _: &[std::path::PathBuf]) {}

    pub(super) fn shutdown(self) {}
}

pub(super) fn handle(_: &mut Owner, message: FilesMessage) {
    match message {}
}
