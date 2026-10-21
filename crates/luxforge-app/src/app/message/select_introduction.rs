//! The Select introduction's gestures.
use crate::state::select_introduction::Closing;

/// One gesture on the Select introduction. Handled in `app/select_introduction.rs`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IntroductionMessage {
    /// The title bar's ?: show the sheet again.
    Reopen,
    /// Close it, and how; Add a folder… then opens the folder dialog.
    Close(Closing),
}
