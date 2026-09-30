//! The catalog in the Select workspace (TASK-022), carried inside the Select workspace's message as
//! [`SelectMessage::Catalog`](super::select::SelectMessage::Catalog).
use crate::state::select_catalog::CatalogAction;
use luxforge_core::catalog_types::{CatalogFolders, Collections, ViewRow, ViewSource};

/// What the catalog's gestures and owner answers are, handled in `app/select_catalog.rs`. Every
/// gesture that changes the view sends the whole query to `browse.view`; every organizing gesture
/// sends the request an API client would.
#[derive(Clone, Debug)]
pub(crate) enum CatalogMessage {
    /// A press, a menu choice, a key or typing: what the model says it does.
    Act(CatalogAction),
    /// `folder.list` and `collection.list` answered.
    Lists(Result<Box<(CatalogFolders, Collections)>, String>),
    /// `browse.facets` counted `source` with no filter at the library change `sequence`.
    Totalled {
        source: ViewSource,
        sequence: u64,
        result: Result<u32, String>,
    },
    /// `browse.rows` answered the selection's rows the desktop did not hold, for the view at
    /// `revision`.
    SelectionRows {
        revision: u64,
        ranges: Vec<(u32, u32)>,
        result: Result<Vec<ViewRow>, String>,
    },
}
