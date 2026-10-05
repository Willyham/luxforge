//! The catalog in the Select workspace, carried inside the Select workspace's message as
//! [`SelectMessage::Catalog`](super::select::SelectMessage::Catalog).
use crate::state::select_catalog::{CatalogAction, PresetChoice};
use luxforge_core::catalog_types::{
    CatalogFolders, Collections, EmptyRemovedAnswer, ViewRow, ViewSource,
};
use serde_json::Value;

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
    /// `preset.list` answered, for the Apply preset… menu.
    Presets(Result<Vec<PresetChoice>, String>),
    /// `job.read` answered the batch job `job`.
    BatchRead {
        job: String,
        result: Result<Value, String>,
    },
    /// One `catalog.empty-removed` call answered: its parameters and its answer.
    Emptied(Result<(Value, EmptyRemovedAnswer), String>),
}
