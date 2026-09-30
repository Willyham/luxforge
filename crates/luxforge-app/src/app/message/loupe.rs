//! The loupe (TASK-020), carried inside the Select workspace's message as
//! [`SelectMessage::Loupe`](super::select::SelectMessage::Loupe).

/// The loupe's gestures and answers, handled in `app/loupe.rs`. The loupe's task adds its variants:
/// stepping frames and moments, jumping to a frame, the 100% focus check, compare, picking and its
/// decoded frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LoupeMessage {
    /// `Space` or `E` over Select's grid: show the active frame in the loupe.
    Open,
    /// `Esc` in the loupe: back to the grid.
    Close,
}
