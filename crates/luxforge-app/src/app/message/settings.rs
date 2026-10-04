//! The Settings sheet.
use crate::state::settings::{GeneralPreferences, SettingsTab};
use luxforge_core::flags::FlagList;
use serde_json::Value;

/// One Settings sheet gesture or owner answer. Handled in `app/settings.rs`.
#[derive(Clone, Debug)]
pub(crate) enum SettingsMessage {
    /// Open the sheet at a tab and read the preferences and the flags.
    Open(SettingsTab),
    Close,
    /// Cmd+, or Ctrl+,: open the sheet, or close it when it is open.
    Toggle,
    /// `flags.list` and `preferences.read` answered.
    Listed {
        flags: Result<FlagList, String>,
        preferences: Result<GeneralPreferences, String>,
    },
    /// Turn Auto collapse history on or off through `preferences.set`.
    SetAutoCollapse(bool),
    /// `preferences.set` answered the Auto collapse history write in flight: the preferences it
    /// left and the request id its event carries, which the event sync then skips.
    PreferencesSaved(Result<(GeneralPreferences, String), String>),
    /// Change a flag to `value`, or reset it to its default for `None`, through `flags.set`.
    Set {
        flag: String,
        value: Option<Value>,
    },
    /// `flags.set` answered the write in flight: the flags it left and the request id its event
    /// carries, which the event sync then skips.
    Saved(Result<(FlagList, String), String>),
    /// A number field's text, as typed.
    NumberText {
        flag: String,
        text: String,
    },
    /// Enter in a number field: send its text when it is a value the flag takes.
    NumberSubmit(String),
}
