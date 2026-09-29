//! Export.
use crate::{app::export::ExportChoice, app::tasks::CallError};
use serde_json::Value;

/// Exporting the displayed entry as a JPEG, and the owner's answers. Handled in `app/export.rs`.
#[derive(Clone, Debug)]
pub(crate) enum ExportMessage {
    /// Export the displayed entry: ask `export.plan` for its suggested name, choose the destination
    /// in the native save dialog and send `export.jpeg`.
    Start { keep_metadata: bool },
    /// The plan answered and a destination was chosen, or `None` when the dialog was cancelled.
    Chosen(Result<Option<Box<ExportChoice>>, String>),
    /// `export.jpeg` answered: the queued job, or the refusal with its code.
    Queued(Result<Value, CallError>),
    /// Read the running job again; produced only while one is queued or running.
    Poll,
    /// `job.read` answered for the job it names.
    Read {
        job_id: String,
        result: Result<Value, String>,
    },
}
