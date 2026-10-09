//! Usage statistics: the startup notice and the Settings sheet's Privacy tab.
use crate::state::usage::UsageRead;

/// One usage statistics gesture or owner answer. Handled in `app/usage.rs`.
#[derive(Clone, Debug)]
pub(crate) enum UsageMessage {
    /// `usage.prompt.claim` answered: whether this launch shows the notice, and the consent.
    Claimed(Result<(bool, UsageRead), String>),
    /// The notice's Share statistics.
    Share,
    /// The notice's No thanks, or Escape while it is on screen.
    Decline,
    /// `usage.read` answered.
    Read(Result<UsageRead, String>),
    /// The Privacy switch turned on or off, written through `usage.set`.
    Set(bool),
    /// `usage.set` answered the write in flight: the consent it left and the request id its event
    /// carries, which the event sync then skips.
    Saved(Result<(UsageRead, String), String>),
    /// Open or close the Privacy tab's What is sent section.
    Details(bool),
}
