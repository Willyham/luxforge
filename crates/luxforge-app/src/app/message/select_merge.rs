//! HDR merges in the Select workspace, carried inside the Select workspace's message as
//! [`SelectMessage::Merge`](super::select::SelectMessage::Merge).
use crate::app::select_merge::StartAnswer;
use crate::state::select_merge::{MergeAction, MergeRequest};
use luxforge_core::catalog_types::MergePlan;
use serde_json::Value;

/// What the merge gestures and owner answers are, handled in `app/select_merge.rs`.
#[derive(Clone, Debug)]
pub(crate) enum MergeMessage {
    /// A press in the menu, the form, the pending row or the Info panel.
    Act(MergeAction),
    /// A moment header's button, by the grid's moment number and the header's action index.
    Header { moment: u32, action: u32 },
    /// A press on a result's stack badge: its cell's item.
    Disclosure(u32),
    /// The pending row's button: Cancel, or Dismiss once the merge failed.
    Pending,
    /// `merge.plan` answered for the form's options numbered `serial`.
    Planned {
        serial: u64,
        result: Result<Box<MergePlan>, String>,
    },
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
