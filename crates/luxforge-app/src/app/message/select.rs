//! The Select workspace.
use crate::state::select::{QueryChange, SelectMenu, SelectPanel, Shown};
use luxforge_core::{
    ClientSession,
    catalog_types::{EventList, Facets, ViewRows, ViewSource, ViewSummary},
};
use luxforge_ui::GridPress;
use std::path::PathBuf;

/// An arrow key's direction in the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Left,
    Right,
    Up,
    Down,
}

/// Browsing files and the catalog in the Select workspace, and the owner's answers. Handled in
/// `app/select.rs`. Every gesture that changes the view sends the whole query to `browse.view`; every
/// selection gesture sends `browse.select`.
#[derive(Clone, Debug)]
pub(crate) enum SelectMessage {
    /// Show a workspace: the title bar's switch, or `G` in Develop.
    Switch(Shown),
    /// The sources panel's search text, which `event.list` answers.
    Search(String),
    /// `event.list` answered.
    Events(Result<EventList, String>),
    /// View a source: an event, the folder browsed on disk or a catalog view.
    Source(ViewSource),
    /// Browse a folder…: the native folder dialog, and what it chose.
    BrowseFolder,
    FolderPicked(Option<PathBuf>),
    /// `index.refresh` of the folder answered with its job, or refused.
    Reading(Result<String, String>),
    /// The reading folder's job is read again: the timer that exists while the job runs.
    ReadPoll,
    /// `job.read` for the reading folder's job answered.
    ReadAnswered(Result<serde_json::Value, String>),
    /// One change of the filter bar, the Group chip or the sort.
    Change(QueryChange),
    /// Open a chip's or the sort's menu, or close the one open.
    Menu(Option<SelectMenu>),
    /// `browse.view` and the session after it answered, for the evaluation numbered `serial`.
    Viewed {
        serial: u64,
        result: Result<Box<(ViewSummary, ClientSession)>, String>,
    },
    /// `browse.facets` answered, for the evaluation numbered `serial`.
    Faceted {
        serial: u64,
        result: Result<Facets, String>,
    },
    /// `browse.rows` answered one block of the view at `revision`.
    Rows {
        revision: u64,
        from: u32,
        result: Result<ViewRows, String>,
    },
    /// The grid's scroll offset, its size and a press on a cell.
    Scrolled(f32),
    Viewport(iced::Size),
    Press(GridPress),
    /// An arrow key: move the active item, and with Shift extend the selection to it.
    Move {
        step: Step,
        extend: bool,
    },
    /// Cmd+A and Cmd+D.
    SelectAll,
    SelectNone,
    /// `S`: collapse or expand the active burst.
    Collapse,
    /// The size slider.
    CellWidth(f32),
    /// A title bar toggle, or `Tab` for both.
    TogglePanel(SelectPanel),
    TogglePanels,
    /// The session read after the owner woke the desktop, which says whether the view went stale.
    Checked(Result<Box<ClientSession>, String>),
    /// The grid's decoded previews: a batch of `preview.read` answers, or their signal.
    Previews(crate::app::select_previews::SelectPreviewMessage),
    /// The loupe, entered from the grid.
    Loupe(crate::app::message::loupe::LoupeMessage),
}
