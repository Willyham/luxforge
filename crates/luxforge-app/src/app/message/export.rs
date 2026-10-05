//! Export.
use crate::{app::export::ExportChoice, app::tasks::CallError};
use serde_json::Value;

/// Exporting the displayed entry as a JPEG, and the owner's answers. Handled in `app/export.rs`.
#[derive(Clone, Debug)]
pub(crate) enum ExportMessage {
    /// Export the displayed entry: ask `export.plan` for its suggested name, choose the destination
    /// in the native save dialog and send `export.jpeg`, with `reference: true` when `reference`
    /// asks for the reference renderer's export.
    Start {
        keep_metadata: bool,
        reference: bool,
    },
    /// The plan answered and a destination was chosen, or `None` when the dialog was cancelled.
    Chosen(Result<Option<Box<ExportChoice>>, String>),
    /// `export.jpeg` answered: the queued job, or the refusal with its code.
    Queued(Result<Value, CallError>),
    /// What the export's reader read of the job it names: its first record, a record that differs
    /// from the one sent before it, the job's end, or a read that failed. The reader sends
    /// nothing for a read that finds the job as the one before it did.
    Read {
        job_id: String,
        result: Result<Value, String>,
    },
}
