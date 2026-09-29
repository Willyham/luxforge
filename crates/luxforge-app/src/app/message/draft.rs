//! The core draft lifecycle of the open gesture.
use crate::{app::draft::GestureId, app::tasks::Refresh};
use luxforge_core::DraftId;

/// The core draft lifecycle of the open gesture: the three decisions a person makes about it, and
/// the one owner answer that arrives as a message. `draft.begin`, `draft.set`, `draft.reapply` and
/// `draft.cancel` have no message: each is answered in the update that sends it. The commit's
/// answer names the gesture and the core draft it belongs to, so an answer for a gesture that has
/// since ended is recognised and dropped rather than taken up by a newer one.
#[derive(Clone, Debug)]
pub(crate) enum DraftMessage {
    /// Release, Enter or Apply: commit the gesture once.
    Commit,
    /// Escape, Cancel or the Changed elsewhere notice's Discard: commit nothing.
    Cancel,
    /// The Changed elsewhere notice's Reapply.
    Reapply,
    /// `draft.commit` answered. `None` is a no-op outcome: the gesture returned to its start, so
    /// there is no entry and no history to refresh.
    Committed {
        gesture: GestureId,
        draft: DraftId,
        result: Result<Option<Box<Refresh>>, String>,
    },
}
