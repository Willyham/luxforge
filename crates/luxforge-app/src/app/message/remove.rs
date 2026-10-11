//! Remove, the Corrections band's fill mode: its controls, its strip and the owner's answers.
//! Handled in `app/remove.rs`.
use crate::app::tasks::CallError;
use luxforge_core::{DecodedPreview, jobs::JobRecord};
use serde_json::Value;
use std::sync::Arc;

/// Which request the Remove menu's Copy as JSON copies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoveRequest {
    Generate,
    Regenerate,
    Accept,
    Cancel,
    /// The chosen candidate's preview read.
    Preview,
}

impl RemoveRequest {
    pub(crate) const ALL: [Self; 5] = [
        Self::Generate,
        Self::Regenerate,
        Self::Accept,
        Self::Cancel,
        Self::Preview,
    ];

    /// The menu's line for it.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Generate => "Copy Generate request",
            Self::Regenerate => "Copy Regenerate request",
            Self::Accept => "Copy Accept request",
            Self::Cancel => "Copy Cancel request",
            Self::Preview => "Copy preview read request",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum RemoveMessage {
    /// The kind control's Remove segment: the next press paints the selection.
    Choose,
    /// The Fill setting chose this tier.
    Tier(String),
    /// The Replace prompt's text, as typed.
    Prompt(String),
    /// Generate (or Replace) for the selection.
    Generate,
    /// Three more candidates for the same selection, from the next seed.
    Regenerate,
    /// Stop the generation running on the photograph.
    Cancel,
    /// Preview this candidate on the canvas.
    Candidate { job_id: String, candidate: u8 },
    /// Commit the chosen candidate.
    Accept,
    /// Install a model file through the capability block's install and its consent notice.
    Download { module_id: String, resource: String },
    /// Open Settings › Experiments, where the tier's flag is.
    Experiments,
    /// The group's menu opened or closed.
    Menu(bool),
    /// Copy one request the group's controls send.
    Copy(RemoveRequest),
    /// The fill task answered the start in flight: its job, or why it was refused.
    Started(Result<Value, CallError>),
    /// `job.cancel` answered.
    Cancelled {
        job_id: String,
        result: Result<(), String>,
    },
    /// A read of one fill job, from its reader.
    Polled {
        job_id: String,
        result: Result<Box<JobRecord>, String>,
    },
    /// One candidate's preview, read and decoded off the update loop.
    Preview {
        job_id: String,
        candidate: u8,
        result: Result<Arc<DecodedPreview>, String>,
    },
}
