//! HDR merges in the Select workspace, carried inside the Select workspace's message as
//! [`SelectMessage::Merge`](super::select::SelectMessage::Merge).
use crate::app::select_merge::{OpenAnswer, PreviewJob, StartAnswer};
use crate::state::select_merge::{MergeAction, MergeRequest};
use iced::widget::image::Handle;
use luxforge_core::catalog_types::MergePlan;
use serde_json::Value;
use std::path::PathBuf;

/// What the merge gestures and owner answers are, handled in `app/select_merge.rs`.
#[derive(Clone, Debug)]
pub(crate) enum MergeMessage {
    /// A press in the menu, the preview sheet, the pending row or the Info panel.
    Act(MergeAction),
    /// A moment header's button, by the grid's moment number and the header's action index.
    Header { moment: u32, action: u32 },
    /// A press on a result's stack badge: its cell's item.
    Disclosure(u32),
    /// The pending row's button: Cancel, or Dismiss once the merge failed.
    Pending,
    /// ⌥ pressed or let go: a merge action merges at once with the last settings while it is
    /// held.
    Option(bool),
    /// `merge.plan` answered for the menu numbered `serial`.
    Planned {
        serial: u64,
        result: Result<Box<MergePlan>, String>,
    },
    /// The preview sheet's plan and `merge.preview` answered, for the sheet's task `serial`.
    Opened {
        serial: u64,
        answer: Box<OpenAnswer>,
    },
    /// A `merge.preview-update` answered with its job, for the sheet's task `serial`.
    Updated {
        serial: u64,
        result: Result<PreviewJob, String>,
    },
    /// The open preview's job as `job.wait` read it.
    PreviewWatched {
        job: String,
        result: Result<Value, String>,
    },
    /// A revision's picture decoded for display.
    Picture {
        path: PathBuf,
        result: Result<Handle, String>,
    },
    /// `merge.preview-close` answered.
    Closed(Result<(), String>),
    /// A merge's start answered: its plan, the folder it made and its job, or why not.
    Started {
        request: Box<MergeRequest>,
        result: Result<Box<StartAnswer>, String>,
    },
    /// The running merge's job as `job.wait` read it.
    Watched {
        job: String,
        result: Result<Value, String>,
    },
}
