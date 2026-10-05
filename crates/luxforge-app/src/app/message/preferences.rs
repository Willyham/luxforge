//! The desktop's preference writer and its reads of the preferences.
use crate::state::preferences::Preferences;

/// An owner answer for the preference writer. Handled in `app/preferences.rs`.
#[derive(Clone, Debug)]
pub(crate) enum PreferenceMessage {
    /// `preferences.set` answered the change in flight: the preferences it left and the request id
    /// its event carries, which the event sync then skips; or why it was refused.
    Saved(Result<(Preferences, String), String>),
    /// `preferences.read` answered a read made for another client's change.
    Read(Result<Preferences, String>),
}
