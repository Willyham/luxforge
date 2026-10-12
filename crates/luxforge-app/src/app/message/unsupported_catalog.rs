//! The unsupported-catalog sheet's messages: its two choices, its releases link, the new catalog's
//! start and an evidence run's capture of the sheet.
use crate::app::Boot;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug)]
pub(crate) enum UnsupportedCatalogMessage {
    /// Start a new catalog was pressed.
    Start,
    /// The new catalog is open and remembered, or why it could not be.
    Started(Result<Started, String>),
    /// Quit was pressed, or the window asked to close.
    Quit,
    /// Open releases page was pressed.
    OpenReleases,
    /// The platform opener's answer for the releases page.
    ReleasesOpened(Result<(), String>),
    /// A window frame passed while an evidence run waits to capture the sheet.
    Frame,
    /// The evidence run's readback of the sheet.
    Captured(iced::window::Screenshot),
    /// The evidence run's capture is written, or why it was not.
    Saved(Result<(), String>),
}

/// A new catalog, opened by its owner off the update loop: the editor's boot over it, taken once.
#[derive(Clone)]
pub(crate) struct Started {
    pub(crate) path: PathBuf,
    pub(crate) boot: Arc<Mutex<Option<Boot>>>,
    /// Why the new catalog could not be remembered, when it could not.
    pub(crate) unremembered: Option<String>,
}

impl std::fmt::Debug for Started {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Started")
            .field("path", &self.path)
            .field("unremembered", &self.unremembered)
            .finish_non_exhaustive()
    }
}
