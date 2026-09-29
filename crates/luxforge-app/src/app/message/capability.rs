//! Module capabilities.
use crate::app::capabilities::Answer;
use luxforge_core::jobs::JobRecord;
use serde_json::Value;

/// One capability gesture on a module's block, its task control or the consent notice, or an owner
/// answer the capability driver started. Every gesture becomes the same owner requests an
/// independent JSON client sends; the text a person is typing stays here until it is committed.
#[derive(Clone, Debug)]
pub(crate) enum CapabilityMessage {
    /// Text typed into a number or text setting.
    FieldText {
        module_id: String,
        field: String,
        text: String,
    },
    /// Enter in a typed setting: commit its text.
    FieldCommit {
        module_id: String,
        field: String,
    },
    /// A toggle or a choice, or a double-click that returns a number to its default: commit this
    /// value at once.
    FieldValue {
        module_id: String,
        field: String,
        value: Value,
    },
    Install {
        module_id: String,
        resource: String,
    },
    Remove {
        module_id: String,
        resource: String,
    },
    Cancel {
        module_id: String,
        job: String,
    },
    /// Withdraw every live grant of the module.
    RevokeAll(String),
    RunTask {
        module_id: String,
        task: String,
    },
    /// Commit a successful task's artifact through the task's declared apply action.
    Apply {
        module_id: String,
        task: String,
    },
    /// Copy the `task.<id>` request the task control would send.
    CopyTaskRequest {
        module_id: String,
        task: String,
    },
    /// Allow (`true`) or Don't allow on the open consent notice.
    Consent(bool),
    /// An owner round trip the driver started has answered.
    Answered(Box<Answer>),
    /// Read the tracked live jobs again; produced only while one is queued or running.
    Poll,
    /// What `job.read` answered for each polled job, by module and job.
    Polled(Vec<(String, String, Result<JobRecord, String>)>),
}
