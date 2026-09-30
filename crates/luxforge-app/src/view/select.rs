//! The Select workspace's regions ([event board](../../../../docs/design/catalog/event.png)): the
//! sources panel, the grouped virtualized grid or the loupe, the Info panel and the floating strip,
//! drawn from `state/select.rs`'s model with the Develop workspace's tokens. **Lane D (views and
//! desktop)** owns it. Like every view it reads only its model.
#![allow(
    dead_code,
    reason = "catalog contracts: lane D draws it when the workspace switch lands"
)]

use crate::{app::message::Message, state::select::SelectModel};
use iced::{Element, widget::Space};

/// The Select workspace's middle row, drawn in place of the Develop workspace's while Select is
/// shown.
pub(crate) fn select(_: &SelectModel) -> Element<'_, Message> {
    Space::new().into()
}
