//! History and versions: undo, redo and restore, selecting an entry for preview and returning to
//! current, holding the Original for comparison, loading older rows, and naming versions.
use super::tasks::versions_task;
use super::{Editor, message::Message};
use iced::Task;
use serde_json::Value;

impl Editor {
    pub(super) fn version_command(&mut self, method: &'static str, params: Value) -> Task<Message> {
        let Some(state) = &self.state else {
            return Task::none();
        };
        if self.busy {
            return Task::none();
        }
        let asset = state.asset.id.clone();
        self.busy = true;
        self.status = format!("Running {method}…");
        versions_task(self.owner.clone(), self.client, asset, method, params)
    }
}
