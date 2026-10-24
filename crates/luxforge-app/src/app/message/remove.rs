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
    /// What the group's setup button sends: Set up, Resume, Try again, Add or Cancel.
    Setup,
    /// Object: the last `query.pick-object` the gesture sent.
    Pick,
    /// Object: the `task.select-object` each selected object was picked with.
    Select,
}

impl RemoveRequest {
    pub(crate) const ALL: [Self; 6] = [
        Self::Generate,
        Self::Regenerate,
        Self::Accept,
        Self::Cancel,
        Self::Preview,
        Self::Setup,
    ];
    /// The Object group's menu: the picks and the selects before the fill's own.
    pub(crate) const OBJECT: [Self; 8] = [
        Self::Pick,
        Self::Select,
        Self::Generate,
        Self::Regenerate,
        Self::Accept,
        Self::Cancel,
        Self::Preview,
        Self::Setup,
    ];

    /// The menu's line for it.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Generate => "Copy Generate request",
            Self::Regenerate => "Copy Regenerate request",
            Self::Accept => "Copy Accept request",
            Self::Cancel => "Copy Cancel request",
            Self::Preview => "Copy preview read request",
            Self::Setup => "Copy setup request",
            Self::Pick => "Copy pick request",
            Self::Select => "Copy select requests",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) enum RemoveMessage {
    /// The kind control's Remove segment: the next press paints the selection.
    Choose,
    /// The kind control's Object segment: the next click picks an object to remove.
    ChooseObject,
    /// Object's Paint shadow: the brush is armed for one stroke, the fill's effect region (or
    /// put down again).
    Shadow,
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
    /// A setup button: install the setup, through the setup sheet when a grant is missing, or
    /// cancel each of its install jobs.
    Setup(crate::state::fill_setup::SetupAction),
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
