//! The Select workspace's view model: the title bar's workspace switch and Develop N, the sources
//! panel, the grouped grid's window and the loupe, the Info panel and the status bar's line, as
//! plain data derived from the inputs after every message. **Lane D (views and desktop)** owns it.
//! Like every view model it names no framework type, no widget and no view.
#![allow(
    dead_code,
    reason = "catalog contracts: lane D fills this model as it lands"
)]

use super::Inputs;

/// What the Select workspace shows. Lane D adds its regions' models.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct SelectModel {}

/// The Select workspace's model, derived from the inputs.
pub(crate) fn derive(_: &Inputs<'_>) -> SelectModel {
    SelectModel {}
}
