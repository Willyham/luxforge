//! Developing picks — Develop N's confirmation in Select — and Develop's development set: its
//! filmstrip and moving through it. Handled in `app/develop.rs`.
use crate::app::loupe_frames::LoupeFramesMessage;
use crate::app::select_previews::SelectPreviewMessage;
use crate::state::develop::SetPhoto;
use luxforge_core::catalog_types::{CatalogFolderId, CatalogFolders, DevelopPlan};
use serde_json::Value;

/// One gesture on developing picks or the development set, or an owner answer for one.
#[derive(Clone, Debug)]
pub(crate) enum DevelopMessage {
    /// Develop N, or `Cmd+Return`: open the confirmation over the picks in view.
    Open,
    /// A double-click on a photograph of a catalog view, by its position: Develop on it, with the
    /// view's photographs as the set.
    OpenAt(u32),
    /// `pick.plan` and `folder.list` answered for the confirmation numbered `serial`.
    Planned {
        serial: u64,
        result: Result<Box<(DevelopPlan, CatalogFolders)>, String>,
    },
    /// The name typed into an event's new-folder field.
    Name { event: usize, text: String },
    /// Open an event's Or add to an existing folder menu, or close the one open.
    Menu(Option<usize>),
    /// An existing folder chosen for an event.
    Existing {
        event: usize,
        folder: CatalogFolderId,
    },
    /// Whether a card's picks use their copies in indexed folders.
    Copies(bool),
    /// Develop, or Return: send `pick.develop` as the confirmation stands.
    Confirm,
    /// Cancel, or Escape: close the confirmation, changing nothing.
    Cancel,
    /// `pick.develop` answered with its job, or refused.
    Started(Result<String, String>),
    /// The Develop's job ended: its `job.read` record.
    Ended {
        job: String,
        result: Result<Value, String>,
    },
    /// A view's photographs were read into the set numbered `serial`, with where the photograph it
    /// was opened on is among them.
    /// The last viewed catalog folder read in an independent browse session.
    FolderSetRead {
        serial: u64,
        result: Result<(Vec<SetPhoto>, usize), String>,
    },
    SetRead {
        serial: u64,
        result: Result<(Vec<SetPhoto>, usize), String>,
    },
    /// `←` or `→` in Develop, or the filmstrip's previous and next buttons.
    Step(isize),
    /// A press on the filmstrip's cell of the set's photograph at this index.
    Show(usize),
    /// `Cmd+Option+F`: collapse or expand the filmstrip.
    Collapse,
    /// The large previews Develop decodes ahead: a batch of `preview.read` answers.
    Frames(LoupeFramesMessage),
    /// The filmstrip's grid previews: a batch of `preview.read` answers.
    Strip(SelectPreviewMessage),
    /// This seam's signal: a decode landed, or the owner wrote a preview this client waits on.
    Woken,
}
