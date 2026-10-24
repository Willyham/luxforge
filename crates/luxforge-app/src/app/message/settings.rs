//! The Settings sheet.
use crate::state::{
    agents::AgentsRead,
    preferences::{GeneralPreference, GeneralValue, Preferences},
    settings::SettingsTab,
};
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
        preferences: Result<Preferences, String>,
    },
    /// `flags.list` answered a read of the flags alone: at launch, and when another client changed
    /// a flag while the sheet is closed (`change`), so what a live flag gates follows it.
    Flags {
        flags: Result<FlagList, String>,
        change: bool,
    },
    /// One gesture on a General row's control, stored through the desktop's preference writer.
    SetGeneral(GeneralPreference, GeneralValue),
    /// The Catalog row's folder dialog answered: the folder chosen, or `None` when it was
    /// cancelled.
    CatalogFolder(Option<std::path::PathBuf>),
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
    /// One Agents tab gesture or owner answer. Handled in `app/agents.rs`.
    Agents(AgentsMessage),
}

/// One Agents tab gesture or owner answer.
#[derive(Clone, Debug)]
pub(crate) enum AgentsMessage {
    /// `session.clients` answered, with the server beside this build as it was looked for.
    Read(Box<AgentsRead>),
    /// The segmented choice chose the client at this index of `Client::ALL`.
    Choose(usize),
    /// Copy: the chosen client's configuration to the system clipboard.
    Copy,
    /// Disconnect on a connected client's row.
    Disconnect(u64),
    /// `session.disconnect` answered.
    Disconnected {
        client_id: u64,
        result: Result<(), String>,
    },
}
