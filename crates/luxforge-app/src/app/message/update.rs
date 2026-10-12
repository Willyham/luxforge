//! Automatic updates: the offer in the workspace, Settings › General's Updates group and the
//! palette's Check for Updates.
use crate::{
    app::tasks::CallError,
    state::update::{NoticeButton, UpdateRead},
};

/// One update gesture or owner answer. Handled in `app/update.rs`.
#[derive(Clone, Debug)]
pub(crate) enum UpdateMessage {
    /// `update.status` answered a read.
    Read(Result<UpdateRead, String>),
    /// Check now in the Updates group, or the palette's Check for Updates: `update.check`.
    Check,
    /// `update.check` answered the check this window asked for: the status it ended with.
    Checked(Result<UpdateRead, CallError>),
    /// One of the offer's buttons.
    Notice(NoticeButton),
    /// `update.install` answered: the status it left and the request id its event carries.
    Installed(Result<(UpdateRead, String), CallError>),
    /// `update.skip` answered: the status it left and the request id its event carries.
    Skipped(Result<(UpdateRead, String), String>),
    /// The platform opener handed the release page to the browser, or could not.
    Opened(Result<(), String>),
    /// The Updates group's Check for updates automatically, through `preferences.set`.
    SetChecks(bool),
    /// The Updates group's Install updates automatically, through `preferences.set`.
    SetInstallAutomatically(bool),
}
