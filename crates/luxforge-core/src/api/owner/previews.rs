//! **Lane B (previews)** on the owner: the preview lane's handle (its priority queue and workers,
//! started on first use), what they post back (a finished preview or region, progress), each
//! client's wanted previews for prioritizing, and the handlers of `preview.read` and
//! `preview.region` (`crate::catalog_types::api`). The preview lane itself is `crate::previews`.
#![allow(dead_code, reason = "catalog contracts: lane B fills this as it lands")]

use super::{ClientId, Owner, catalog::Poster};
use crate::{JobId, activity::ActivityBoard};
use std::sync::Arc;

/// Lane B's state on the owner.
pub(super) struct PreviewsLane {}

/// What lane B's workers post back.
pub(super) enum PreviewsMessage {}

impl PreviewsLane {
    pub(super) fn new(_: Poster, _: Arc<ActivityBoard>) -> Self {
        Self {}
    }

    pub(super) fn disconnect(&mut self, _: ClientId) {}

    /// `job.cancel` cancelled one of this lane's jobs in the job table.
    pub(super) fn cancelled(&mut self, _: &JobId) {}

    pub(super) fn shutdown(self) {}
}

pub(super) fn handle(_: &mut Owner, message: PreviewsMessage) {
    match message {}
}
