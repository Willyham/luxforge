//! Merges in the Select workspace, carried inside the Select workspace's message as
//! [`SelectMessage::Merge`](super::select::SelectMessage::Merge).
use crate::app::select_merge::{OpenAnswer, SheetJob, StartAnswer};
use crate::state::select_merge::{MergeAction, MergeRequest};
use iced::widget::image::Handle;
use crate::state::select_merge::PanoramaFacts;
use luxforge_core::catalog_types::{MergeId, MergeKind, MergePlan};
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
    /// `merge.plan` of `kind` answered for the menu numbered `serial`.
    Planned {
        serial: u64,
        kind: MergeKind,
        result: Result<Box<MergePlan>, String>,
    },
    /// `merge.inspect` answered for the panorama result the Info panel shows.
    Inspected {
        merge_id: MergeId,
        result: Result<PanoramaFacts, String>,
    },
    /// The preview sheet's plan and `merge.preview` answered, for the sheet's task `serial`.
    Opened {
        serial: u64,
        answer: Box<OpenAnswer>,
    },
    /// A `merge.preview-update` answered with its job, for the sheet's task `serial`.
    Updated {
        serial: u64,
        result: Result<SheetJob, String>,
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
