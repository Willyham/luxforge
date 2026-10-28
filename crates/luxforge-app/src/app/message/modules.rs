//! The person's module layout.
use crate::state::module_layout::Move;
use luxforge_core::{
    Toolset,
    module_layout::{LayoutView, ListedModule},
};

/// What module discovery read: `module.list`'s modules with their placements and switches, and the
/// layout `module.layout.read` answered.
#[derive(Clone, Debug)]
pub(crate) struct Discovery {
    pub(crate) modules: Vec<ListedModule>,
    pub(crate) layout: LayoutView,
}

/// A layout write of this desktop's and what it answered.
#[derive(Clone, Debug)]
pub(crate) struct Written {
    /// The layout after the change, as every layout write answers.
    pub(crate) layout: LayoutView,
    /// The request its `module.layout` event carries, which the event sync then skips.
    pub(crate) request: String,
    /// What the status bar says once it is adopted.
    pub(crate) status: String,
}

/// One gesture on the module layout, or the owner's answer to a layout read or write. Handled in
/// `app/module_layout.rs`.
#[derive(Clone, Debug)]
pub(crate) enum ModulesMessage {
    /// Discovery answered: the modules and the layout, read together at launch.
    Discovered(Result<Box<Discovery>, String>),
    /// `module.layout.read` answered, after another client's change.
    Read(Result<LayoutView, String>),
    /// Turn a module on (`module.enable`) or off (`module.disable`).
    SetEnabled { module: String, enabled: bool },
    /// Move a module within a toolset's order: the client computes the whole order and sends it
    /// with `module.order.set`.
    Move {
        toolset: Toolset,
        module: String,
        to: Move,
    },
    /// The open band's menu's move, from ⌥↑ or ⌥↓.
    MoveOpenBand(Move),
    /// A write answered.
    Written(Result<Box<Written>, String>),
    /// Copy as JSON request on a band's menu: every request the menu sends.
    CopyBand(String),
    /// Customize panel…, Customize tools… and the footer's Customize…: open where the person
    /// arranges `toolset`, with `module` selected when the gesture came from one. This is the one
    /// entry point every Customize gesture goes through.
    Customize {
        toolset: Toolset,
        module: Option<String>,
    },
}
