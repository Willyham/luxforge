//! Developing picks — Develop N's confirmation in Select — and Develop's folder: its filmstrip
//! and moving through it. Handled in `app/develop.rs`.
use crate::app::loupe_frames::LoupeFramesMessage;
use crate::app::select_previews::SelectPreviewMessage;
use crate::state::develop::FolderRead;
use luxforge_core::catalog_types::{CatalogFolderId, CatalogFolders, DevelopPlan};
use serde_json::Value;

/// One gesture on developing picks or the filmstrip, or an owner answer for one.
#[derive(Clone, Debug)]
pub(crate) enum DevelopMessage {
    /// Develop N, or `Cmd+Return`: open the confirmation over the picks in view.
    Open,
    /// A double-click on a photograph of a catalog view, by its position: Develop on it, with its
    /// own catalog folder as the Develop folder.
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
    /// The Develop folder was read for the filmstrip numbered `serial`, through a client of its own.
    StripRead {
        serial: u64,
        result: Result<FolderRead, String>,
    },
    /// The folder holding the photographs a Develop N developed, or a file opened, was found and
    /// read for the filmstrip numbered `serial`; none when none of them was found.
    FolderFound {
        serial: u64,
        result: Result<Option<FolderRead>, String>,
    },
    /// The launch read this catalog's identity and the remembered Develop folder, which is there
    /// when it is this catalog's and still listed.
    Remembered(Result<crate::app::remembered::RememberedFolder, String>),
    /// `←` or `→` in Develop, or the filmstrip's previous and next buttons.
    Step(isize),
    /// A press on the filmstrip's cell of the photograph at this index.
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
