//! The question a macOS release build asks when it runs outside Applications.
use luxforge_install::{Moved, Placement};
use std::path::PathBuf;

/// One gesture on the move question, or an answer from off the update loop. Handled in
/// `app/move_to_applications.rs`.
#[derive(Clone, Debug)]
pub(crate) enum MoveMessage {
    /// Where the running bundle is, or `None` outside one.
    Classified(Option<Placement>),
    /// Move and relaunch, or Try again after a failure.
    Move,
    /// Replace and relaunch, on the confirmation that an existing Luxforge goes to the Trash.
    Replace,
    /// Cancel on that confirmation: the question again.
    Cancel,
    /// Not now: the question goes, and Don't ask again, when on, is stored.
    NotNow,
    /// The Don't ask again switch.
    DontAsk(bool),
    /// The move answered.
    Answered(Result<MoveDone, MoveFailure>),
}

/// A move that happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MoveDone {
    /// The question moved the app itself: the relaunch runs after the quit path.
    Moved(Moved),
    /// `update.move-to-applications` accepted the move: the update port carries it out and runs
    /// the quit path.
    Requested,
}

/// Why a move did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MoveFailure {
    /// A Luxforge is already at this path; replacing it needs the person's confirmation.
    Existing(PathBuf),
    /// Anything else, in words, and its reason's name.
    Failed { reason: String, detail: String },
}
