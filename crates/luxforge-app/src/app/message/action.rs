//! Declared actions.
use serde_json::{Map, Value};

/// Running a declared action, or copying the request one would send. Handled in `app/actions.rs`.
#[derive(Clone, Debug)]
pub(crate) enum ActionMessage {
    /// An analysis action's command and ordinary read-back completed.
    Analysed {
        action: String,
        query: String,
        serial: u64,
        result: Result<Box<crate::app::tasks::Refresh>, String>,
    },
    /// Its read-only explanation, pinned to the committed entry.
    Explained {
        action: String,
        asset: luxforge_core::AssetId,
        entry: luxforge_core::EntryId,
        result: Result<Value, String>,
    },
    /// Run one declared action with a fixed preset over the current field values.
    Run {
        action: String,
        preset: Map<String, Value>,
    },
    /// Copy the JSON request this control would send to the clipboard.
    CopyRequest {
        action: String,
        parameter: Option<String>,
        preset: Option<Map<String, Value>>,
    },
    /// Copy the JSON request the open crop draft's own Apply would send.
    CopyDraftRequest,
    /// Copy the `workspace.set` request this module's picker control would send.
    CopyModeRequest(String),
}
