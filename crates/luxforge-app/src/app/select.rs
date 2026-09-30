//! The Select workspace ([catalog design](../../../../docs/design/catalog.md#workspaces)): browsing
//! cards, folders on disk, events and the catalog, the grouped grid and the loupe, picking, Develop
//! N with its confirmation, and the development set Develop opens with. **Lane D (views and
//! desktop)** owns this seam.
//!
//! The desktop holds no catalog logic: every gesture sends the request its API equivalent sends
//! (`browse.*`, `pick.*`, `library.*`, `preview.*`), the owner holds the view and the selection, and
//! this seam keeps only what it last read and what is in flight. Its view model is
//! `state/select.rs` and its regions `view/select.rs`, within the enforced layering.
#![allow(
    dead_code,
    reason = "catalog contracts: lane D fills this seam as it lands"
)]

use crate::app::{
    Editor,
    message::{Message, select::SelectMessage},
};
use iced::Task;

/// The Select workspace's own state in the editor: what it last read from the owner and what is in
/// flight. Lane D adds its fields.
#[derive(Clone, Debug, Default)]
pub(crate) struct Select {}

impl Editor {
    /// One Select message.
    pub(crate) fn select_update(&mut self, message: SelectMessage) -> Task<Message> {
        match message {}
    }
}
