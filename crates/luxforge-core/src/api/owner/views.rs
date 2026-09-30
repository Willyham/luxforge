//! **Lane D (views)** on the owner: each client's one view — its evaluated item list
//! (`crate::catalog_types::ViewItem`s, 16 bytes each), kept here and never in the session, whose
//! `browse` part (`crate::catalog_types::BrowseSession`) `session.state` reports — dropped when the
//! client disconnects, and the handlers of `event.list`, `browse.view`, `browse.rows`,
//! `browse.facets` and `browse.select` (`crate::catalog_types::api`). Views are evaluated on the
//! owner by `crate::browse`, so this lane posts no messages.
#![allow(dead_code, reason = "catalog contracts: lane D fills this as it lands")]

use super::ClientId;
use crate::{Error, catalog_types::ViewItem};

/// Lane D's state on the owner: every client's view.
#[derive(Default)]
pub(super) struct ViewsLane {}

impl ViewsLane {
    pub(super) fn disconnect(&mut self, _: ClientId) {}

    /// The items selected in `client`'s current view, which a library method's `{kind: selection}`
    /// targets name: `validation` when the client has no view, `conflict` when a later change made
    /// its view stale. Lane D owns the body; lane C calls it.
    pub(super) fn selected(&self, _: ClientId) -> Result<Vec<ViewItem>, Error> {
        Err(Error::validation("this client has no view"))
    }
}
