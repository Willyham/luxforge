//! The event sync's and the owner's answers about the open photograph.
use crate::app::tasks::{RecipeRead, Refresh, SyncResult};
use luxforge_core::ModuleDescriptor;
use std::path::PathBuf;

/// Opening a photograph and the owner's answers the editor adopts as authoritative: a command's
/// read-back, the event sync, the displayed entry's recipe rows and module discovery. Handled in
/// `app/sync.rs`.
#[derive(Clone, Debug)]
pub(crate) enum SyncMessage {
    /// Open the native file picker.
    Open,
    /// The picker closed, with a chosen path or nothing.
    Picked(Option<PathBuf>),
    /// Authoritative state read back after a change.
    Refreshed(Result<Box<Refresh>, String>),
    /// A source-open result tied to the generation that requested it.
    ImportRefreshed(u64, Result<Box<Refresh>, String>),
    /// The displayed entry's layers and masks as the panels read them.
    RecipeDescribed(Result<Box<RecipeRead>, String>),
    /// Every tool control is generated from these; the desktop knows no tool by name.
    ModulesLoaded(Result<Vec<ModuleDescriptor>, String>),
    /// Another client's change reached the owner's event log, so the event sync reads it: the
    /// owner's wake, carried in while an asset is open. Nothing produces it on a timer.
    Changed,
    /// The result of one live-refresh poll.
    Synced(Result<SyncResult, String>),
}
