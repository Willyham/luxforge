//! The Settings sheet.
use crate::state::{
    agents::AgentsRead,
    external_modules::Inspected,
    module_settings::{ModulesFocus, ModulesRead, Move, RowKey},
    preferences::{GeneralPreference, GeneralValue, Preferences},
    settings::SettingsTab,
};
use luxforge_core::{Toolset, flags::FlagList, module_layout::LayoutView};
use serde_json::Value;

/// One Settings sheet gesture or owner answer. Handled in `app/settings.rs`.
#[derive(Clone, Debug)]
pub(crate) enum SettingsMessage {
    /// Open the sheet at a tab and read the preferences and the flags.
    Open(SettingsTab),
    /// Open the sheet at Modules, at one module's home row or one group's first row when given:
    /// the one route in for Customize panel…, Customize tools…, the off-modules footer and the
    /// palette's Customize entries.
    OpenModules(Option<ModulesFocus>),
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
    /// One Modules tab gesture or owner answer. Handled in `app/module_settings.rs`.
    Modules(ModulesMessage),
}

/// One Modules tab gesture or owner answer.
#[derive(Clone, Debug)]
pub(crate) enum ModulesMessage {
    /// `module.list` and `module.layout.read` answered the tab's read.
    Read(Result<Box<ModulesRead>, String>),
    /// A click on a row selects it, and its detail shows.
    Select(RowKey),
    /// A home row's switch: `module.enable` or `module.disable`.
    Switch { module: String, on: bool },
    /// A locked row's lock was pressed: the detail pane says why it is always on.
    Locked(RowKey),
    /// A key while the tab is open.
    Key(ModulesKey),
    /// Reset order on a group: `module.layout.reset {toolset}`.
    ResetOrder(Toolset),
    /// Reset all: `module.layout.reset {}`.
    ResetAll,
    /// The toast's Undo, or Cmd+Z while it shows: write back what the last change replaced.
    Undo,
    /// One step of a reorder by a row's grip.
    Drag(ModuleDragEdit),
    /// The layout write in flight answered: the layout it left and the request id its event
    /// carries, which the event sync then skips.
    Written(Result<(LayoutView, String), String>),
    /// Install module…, from the tab's header, a detail pane or the stale-preview notice: the
    /// system file picker, over Settings › Modules.
    Install,
    /// The picker answered: the package chosen, or `None` when it was cancelled. An evidence run
    /// hands its path in here, so no automated run opens the native picker.
    InstallPicked(Option<std::path::PathBuf>),
    /// `module.package.inspect` answered for the review sheet: the package's review, or the
    /// owner's refusal with its `data.refusal` code.
    Inspected {
        path: std::path::PathBuf,
        result: Result<Box<Inspected>, (String, Option<String>)>,
    },
    /// The review sheet's Install, Replace or Replace anyway: `module.install`.
    ReviewInstall,
    /// Uninstall… on an installed module's detail: the confirmation.
    Uninstall(String),
    /// The confirmation's Uninstall: `module.uninstall`.
    UninstallConfirm,
    /// Cancel, Close or Escape on the review sheet or the confirmation.
    Dismiss,
    /// Return on the review sheet: its confirming button, unless it is the destructive one.
    Return,
    /// Load from folder…, in developer mode: the system folder picker.
    LoadFolder,
    /// The folder picker answered: `module.folder.load` for the folder chosen.
    FolderPicked(Option<std::path::PathBuf>),
    /// Remove on a folder module's detail: `module.folder.remove`.
    RemoveFolder(String),
    /// The lifecycle write in flight answered: the owner's answer and the request id its
    /// `module.installed` event carries, or the refusal's words.
    PackageWritten(Result<(Value, String), String>),
    /// Copy error on a module that didn't load: the loader's reason, to the clipboard.
    CopyError(String),
    /// Relaunch: quit through the ordinary close path and start this desktop again over the same
    /// catalog, so the pending changes apply.
    Relaunch,
}

/// A key the Modules tab takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModulesKey {
    /// ↑ or ↓: select the row before or after.
    Step(isize),
    /// ⌥↑ ⌥↓ move one place, ⌥⇧↑ ⌥⇧↓ to the group's ends.
    Move(Move),
    /// Space turns the selected module on or off.
    Toggle,
}

/// One step of a reorder by a row's grip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ModuleDragEdit {
    /// The grip was pressed.
    Start(RowKey),
    /// The pointer entered a row of the dragged row's group, by module.
    Over(String),
    /// The button came up: one `module.order.set` when the row lands somewhere new.
    End,
    /// Escape, or the pointer leaving the list: nothing is written.
    Cancel,
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
