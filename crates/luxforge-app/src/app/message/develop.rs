//! Developing picks — Develop N's confirmation in Select — and Develop's development set: its
//! filmstrip and moving through it. Handled in `app/develop.rs`.
use crate::app::loupe_frames::LoupeFramesMessage;
use crate::app::select_previews::SelectPreviewMessage;
use crate::state::develop::ReadSet;
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
    Name {
        event: usize,
        text: String,
    },
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
    /// The last viewed catalog folder's photographs, made the owner's development set in an
    /// independent browse session.
    FolderSetRead {
        serial: u64,
        result: Result<ReadSet, String>,
    },
    /// The owner's development set was read into the set numbered `serial`: after this desktop
    /// replaced it, or once another client changed it.
    SetRead {
        serial: u64,
        result: Result<ReadSet, String>,
    },
    /// `←` or `→` in Develop, or the filmstrip's previous and next buttons.
    Step(isize),
    /// A press on the filmstrip's cell of the set's photograph at this index.
    Show(usize),
    Select {
        index: usize,
        command: bool,
        shift: bool,
    },
    SelectAll,
    SelectOnly,
    CellMenu {
        index: usize,
        at: iced::Point,
    },
    /// `Cmd+Option+F`: collapse or expand the filmstrip.
    Collapse,
    /// The large previews Develop decodes ahead: a batch of `preview.read` answers.
    Frames(LoupeFramesMessage),
    /// The filmstrip's grid previews: a batch of `preview.read` answers.
    Strip(SelectPreviewMessage),
    /// This seam's signal: a decode landed, or the owner wrote a preview this client waits on.
    Woken,
}
