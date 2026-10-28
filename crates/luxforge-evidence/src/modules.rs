//! The person's module layout's script steps: the gestures the tools panel's band menus, the mode
//! strip's ⋯ menu, the palette and Settings › Modules send, each through the desktop's own message,
//! and Settings › Modules' external-module gestures: Install module…, the review sheet,
//! Uninstall… and Relaunch.
use serde::{Deserialize, Serialize};

/// The toolsets a step may name: the two a person orders.
pub const MODULE_TOOLSETS: [&str; 2] = ["adjustments", "tools"];

/// Where a move takes a module, as a band's menu and the Modules tab's keys say it.
pub const MODULE_MOVES: [&str; 4] = ["up", "down", "top", "bottom"];

/// One gesture on the module layout. A step that sends a layout write is captured once every
/// write the desktop has outstanding has answered and the owner's `module.layout.read` and
/// `module.list` have been read beside the frame, which its record keeps; a step that writes
/// nothing is captured on the next frame, with the same reads.
///
/// - `{"band_menu": "luxforge.vignette"}` opens a band's menu, as a secondary click on it does.
/// - `"strip_menu"` opens the mode strip's ⋯ menu.
/// - `"close_menu"` closes whichever menu is open.
/// - `{"move": {"module": M, "to": "top"}}` is a band menu's Move item for a tools-panel band.
/// - `{"enabled": {"module": M, "on": false}}` is a band menu's Turn off *Title*, or `on: true`
///   the palette's or a kept band's Enable *Title*.
/// - `{"customize": {"toolset": "adjustments", "module": M}}` is a band menu's Customize panel…,
///   or, without a module, the footer's Customize… (`adjustments`) or the strip menu's Customize
///   tools… (`tools`): each opens Settings › Modules.
/// - `{"tab_move": {"toolset": T, "module": M, "to": "up"}}` selects a row of the open Modules
///   tab and presses ⌥↑ (`up`), ⌥↓ (`down`), ⌥⇧↑ (`top`) or ⌥⇧↓ (`bottom`).
/// - `"reset_all"` presses the open Modules tab's Reset all.
/// - `{"install": PATH}` is Install module… with `PATH` picked: it opens Settings › Modules when
///   it is not open and hands the path to the review sheet as the system file picker would, which
///   an evidence run never opens. The frame is captured once `module.package.inspect` answered.
/// - `"review_install"` presses the review sheet's Install (or Replace), which must send
///   `module.install`.
/// - `{"uninstall": M}` presses Uninstall… in the open Modules tab, which opens its confirmation.
/// - `"confirm_uninstall"` presses the confirmation's Uninstall, which must send
///   `module.uninstall`.
/// - `"dismiss"` presses the open review sheet's or confirmation's Cancel or Close.
/// - `"relaunch"` presses Relaunch: an evidence run records the command it would start and quits
///   nothing.
/// - `{"probed": M}` presses nothing: it waits until the GPU program of the external module `M`,
///   which this launch registered, has been probed, so its state is neither pending nor probing,
///   reading `module.package.list` again every evidence tick. Something must have asked for the
///   probe, such as the view drawing a stack that uses the module; the step otherwise waits until
///   the script's deadline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum ModulesStep {
    BandMenu(String),
    StripMenu,
    CloseMenu,
    Move(ModuleMove),
    Enabled(ModuleSwitch),
    Customize(ModuleCustomize),
    TabMove(ModuleTabMove),
    ResetAll,
    Install(String),
    ReviewInstall,
    Uninstall(String),
    ConfirmUninstall,
    Dismiss,
    Relaunch,
    Probed(String),
}

/// A tools-panel band's move.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleMove {
    pub module: String,
    /// One of [`MODULE_MOVES`].
    pub to: String,
}

/// A module turned on or off.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleSwitch {
    pub module: String,
    pub on: bool,
}

/// A Customize gesture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleCustomize {
    /// One of [`MODULE_TOOLSETS`].
    pub toolset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
}

/// A row of the Modules tab moved by its keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModuleTabMove {
    /// One of [`MODULE_TOOLSETS`].
    pub toolset: String,
    pub module: String,
    /// One of [`MODULE_MOVES`].
    pub to: String,
}

impl From<ModulesStep> for crate::Step {
    fn from(step: ModulesStep) -> Self {
        Self::Modules(step)
    }
}

impl ModulesStep {
    pub(crate) fn validate(&self) -> Result<(), String> {
        let module = |module: &str| super::text(module, "modules step module");
        let to = |to: &str| {
            if MODULE_MOVES.contains(&to) {
                Ok(())
            } else {
                Err(format!(
                    "a modules move goes to one of {}, not {to:?}",
                    MODULE_MOVES.join(", ")
                ))
            }
        };
        let toolset = |toolset: &str| {
            if MODULE_TOOLSETS.contains(&toolset) {
                Ok(())
            } else {
                Err(format!(
                    "a modules step's toolset is one of {}, not {toolset:?}",
                    MODULE_TOOLSETS.join(", ")
                ))
            }
        };
        match self {
            Self::BandMenu(id) | Self::Uninstall(id) | Self::Probed(id) => module(id),
            Self::Install(path) => super::text(path, "modules step install path"),
            Self::StripMenu
            | Self::CloseMenu
            | Self::ResetAll
            | Self::ReviewInstall
            | Self::ConfirmUninstall
            | Self::Dismiss
            | Self::Relaunch => Ok(()),
            Self::Move(step) => {
                module(&step.module)?;
                to(&step.to)
            }
            Self::Enabled(step) => module(&step.module),
            Self::Customize(step) => {
                toolset(&step.toolset)?;
                step.module.as_deref().map_or(Ok(()), module)
            }
            Self::TabMove(step) => {
                toolset(&step.toolset)?;
                module(&step.module)?;
                to(&step.to)
            }
        }
    }
}
