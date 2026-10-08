//! Copy/paste gestures and bounded capture results.
use crate::state::copy_settings::{Chooser, Clipboard, Source, Targets};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(crate) enum CopySettingsMessage {
    Copy {
        choose: bool,
        source: Option<Source>,
    },
    Chosen,
    Check {
        label: String,
        checked: bool,
    },
    CheckMany {
        module: Option<String>,
        edited: bool,
        checked: bool,
    },
    Cancel,
    CopyReport,
    Captured {
        serial: u64,
        result: Result<Arc<Clipboard>, String>,
        previous: bool,
    },
    Inspected {
        serial: u64,
        result: Result<Chooser, String>,
    },
    Paste,
    PasteTo(Targets),
    Previous,
    Confirm,
    BatchStarted {
        kind: crate::state::select_catalog::BatchKind,
        params: serde_json::Value,
        count: u32,
        names: std::collections::BTreeMap<luxforge_core::AssetId, String>,
        result: Result<(String, serde_json::Value), crate::app::tasks::CallError>,
    },
}
