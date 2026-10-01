//! History and versions.
use crate::app::tasks::PreviewPayload;
use luxforge_core::{EntryId, HistoryPage, Version};

/// History and versions: undo, redo, restore, a history selection, the Original held for
/// comparison, older rows and named versions. Handled in `app/history.rs`.
#[derive(Clone, Debug)]
pub(crate) enum HistoryMessage {
    Undo,
    Redo,
    /// Select one history entry for preview.
    Select(EntryId),
    ReturnCurrent,
    Restore,
    /// A history selection or a return to current answered. It set `busy`, and this answer is what
    /// clears it.
    Selected(Result<Box<PreviewPayload>, String>),
    /// Physical keyboard input; a tap toggles and a held key temporarily shows Before.
    CompareKeyPressed {
        uncropped: bool,
    },
    CompareKeyReleased,
    CompareKeyCancelled,
    CompareHoldElapsed(u64),
    /// Hold the Original entry's preview, framed by the displayed entry's geometry (orientation,
    /// straighten and crop) so only the adjustments differ.
    CompareBegin,
    /// Hold the Original entry's preview with its own geometry: the whole, uncropped original.
    CompareUncropped,
    /// Release the compare hold and restore the previous selection.
    CompareEnd,
    /// Toggle the persistent before/after divider; Escape exits it explicitly.
    CompareToggle,
    CompareExit,
    ComparePosition(f32),
    LoadOlder,
    /// An older history page.
    OlderLoaded(Result<HistoryPage, String>),
    /// The version name field's text.
    VersionName(String),
    /// Show or hide the version-naming field the "+" chip reveals.
    ToggleVersionForm,
    SaveVersion,
    DeleteVersion(String),
    /// The named versions after a create or delete, and the request that created or deleted one.
    VersionsLoaded(Result<(Vec<Version>, String), String>),
}
