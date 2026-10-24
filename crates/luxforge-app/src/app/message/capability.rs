//! Module capabilities.
use crate::{
    app::capabilities::Answer,
    state::{fill_setup::SetupAction, models::ModelRequest},
};
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
    /// The Models tab's Unload now: release one loaded model through `module.runtime.unload`.
    Unload {
        module_id: String,
        resource: String,
    },
    /// The Models tab's Remove on a part: remove each of its resources that is installed or
    /// partly downloaded, one `module.resource.remove` each.
    RemoveResources {
        module_id: String,
        resources: Vec<String>,
    },
    /// Copy the requests a Models tab button sends: Remove (one per resource) or Unload now.
    CopyModelRequest {
        module_id: String,
        resources: Vec<String>,
        request: ModelRequest,
    },
    /// A setup button: Set up, Resume, Try again or Download… sends `module.setup.install`, which
    /// opens the setup sheet when a grant is missing; Cancel cancels each of the setup's jobs.
    Setup(SetupAction),
    /// Copy the requests a setup button sends.
    CopySetupRequest(SetupAction),
    Cancel {
        module_id: String,
        job: String,
    },
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
    /// Allow (`true`) or Don't allow on the open consent notice; Download or Not now on the setup
    /// sheet while it is open.
    Consent(bool),
    /// An owner round trip the driver started has answered.
    Answered(Box<Answer>),
    /// What the capability reader read of tracked live jobs, by module and job: a job's first
    /// record, a record that differs from the one sent before it, a job's end, or a read that
    /// failed. The reader sends nothing for a job it finds as it did before, and each entry stands
    /// alone, so a message holds only the entries that changed.
    Polled(Vec<(String, String, Result<JobRecord, String>)>),
}

impl From<SetupAction> for CapabilityMessage {
    /// A setup button's request: the setup install, or the cancel of each of its jobs.
    fn from(action: SetupAction) -> Self {
        Self::Setup(action)
    }
}
