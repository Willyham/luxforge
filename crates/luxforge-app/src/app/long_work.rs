//! Long-running work ([catalog design](../../../../docs/design/catalog.md#long-running-work), P18,
//! TASK-024): every job over a second shown with its progress and Cancel, the busiest one in the
//! status bar, and a progress sheet only in a view with nothing to show yet.
//!
//! **Seam.** Its own update, after-message hook and subscription, each already listed where the
//! editor runs them (`Message::LongWork` routes here; `app/mod.rs` lists [`after_message`] and
//! [`subscription`]); its model is `state/long_work.rs`, derived into `Workspace::long_work`, which
//! both status bars and Select's centre draw. Cancel is `job.cancel` for the job named, as the
//! Performance section's rows and the sheet send it. They do nothing yet: the task builds the
//! behaviour on them.
use crate::app::{
    Before, Editor,
    message::{Message, long_work::LongWorkMessage},
};
use iced::{Subscription, Task};

impl Editor {
    /// One long-running-work message.
    pub(crate) fn long_work_update(&mut self, message: LongWorkMessage) -> Task<Message> {
        match message {
            LongWorkMessage::OpenPerformance
            | LongWorkMessage::Cancel { .. }
            | LongWorkMessage::ContinueInBackground => Task::none(),
        }
    }
}

/// After every message: what long-running work reads, once it reads anything.
pub(super) fn after_message(_: &mut Editor, _: &Before) -> Task<Message> {
    Task::none()
}

/// What long-running work listens to while a job runs, once it has anything.
pub(super) fn subscription(_: &Editor) -> Subscription<Message> {
    Subscription::none()
}
