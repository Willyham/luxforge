//! The core draft lifecycle of the open gesture.
use crate::{
    app::draft::GestureId,
    app::tasks::{Refresh, SetAnswer},
};
use luxforge_core::{Draft, DraftId};

/// The core draft lifecycle of the open gesture: the three decisions a person makes about it, and
/// the owner answers that arrive as messages. `draft.begin` and `draft.cancel` have no message:
/// each is answered in the update that sends it, and so is every `draft.set` and `draft.reapply`
/// that reads no pixel. One whose plan reads one — a colour-limited stroke's seed — and the commit
/// are answered as messages. Each answer names the gesture and the core draft it belongs to,
/// so an answer for a gesture that has since ended is recognised and dropped rather than taken up by
/// a newer one.
#[derive(Clone, Debug)]
pub(crate) enum DraftMessage {
    /// Release, Enter or Apply: commit the gesture once.
    Commit,
    /// Escape, Cancel or the Changed elsewhere notice's Discard: commit nothing.
    Cancel,
    /// The Changed elsewhere notice's Reapply.
    Reapply,
    /// A `draft.set` that read a pixel off the owner answered, with the preview job of the fields
    /// it accepted.
    Set {
        gesture: GestureId,
        draft: DraftId,
        result: Result<Box<SetAnswer>, String>,
    },
    /// A `draft.reapply` that read a pixel off the owner answered.
    Reapplied {
        gesture: GestureId,
        draft: DraftId,
        result: Result<Box<Draft>, String>,
    },
    /// `draft.commit` answered. `None` is a no-op outcome: the gesture returned to its start, so
    /// there is no entry and no history to refresh.
    Committed {
        gesture: GestureId,
        draft: DraftId,
        result: Result<Option<Box<Refresh>>, String>,
    },
}
