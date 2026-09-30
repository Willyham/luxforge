//! The loupe ([catalog design](../../../../docs/design/catalog.md#browsing-at-speed), TASK-020): the
//! active frame of Select's view fitted to the screen with the moment's frames under it, the 100%
//! focus check and compare, with a look-ahead in the direction of travel.
//!
//! **Seam.** The loupe's own update, after-message hook and subscription, each already listed
//! where the editor runs them (`Editor::select_update` routes [`LoupeMessage`]s here, and
//! `app/mod.rs` lists [`after_message`] and [`subscription`]); its state is
//! [`LoupeState`](crate::state::loupe::LoupeState) in Select's state, its model
//! `state/loupe.rs` and its region `view/loupe.rs`. What it gets from Select is
//! [`subject`](crate::state::loupe::subject): the view, the active item and its moment. They do
//! nothing yet: the loupe's task builds its behaviour on them.
use crate::app::{
    Before, Editor,
    message::{Message, loupe::LoupeMessage},
};
use iced::{Subscription, Task};

impl Editor {
    /// One loupe message.
    pub(crate) fn loupe_update(&mut self, message: LoupeMessage) -> Task<Message> {
        match message {
            // The loupe's task opens it over the active frame and closes it back to the grid.
            LoupeMessage::Open | LoupeMessage::Close => Task::none(),
        }
    }

    /// The loupe is open over Select's centre.
    pub(crate) fn loupe_open(&self) -> bool {
        self.select_shown() && self.select.state.loupe.open
    }

    /// A pick of the frames at `positions` through [`Editor::select_pick`] was answered, in the
    /// update that sent it: `succeeded` says whether it recorded (a no-op records nothing and still
    /// succeeds). The loupe's task moves on from here to the next moment after a burst's pick (the
    /// design's P7).
    pub(crate) fn loupe_picked(&mut self, _positions: &[u32], _picked: bool, _succeeded: bool) {}
}

/// After every message: the loupe's look-ahead and decodes, once it has them.
pub(super) fn after_message(_: &mut Editor, _: &Before) -> Task<Message> {
    Task::none()
}

/// What the loupe listens to while it is open, once it has anything.
pub(super) fn subscription(_: &Editor) -> Subscription<Message> {
    Subscription::none()
}
