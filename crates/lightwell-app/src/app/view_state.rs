//! Per-client view state the owner holds in the session: zoom and pan, the side panels, thirds,
//! the canvas mode, the developer gallery page, and the local focus, menu and window facts.
use super::tasks::{pan_task, session_task, workspace_task};
use super::{Editor, message::Message};
use crate::state::tools;
use iced::Task;
use serde_json::{Value, json};

impl Editor {
    /// Pan is session state like zoom, but scroll events arrive faster than round trips complete:
    /// keep one request in flight and only the newest pending position.
    pub(super) fn pan(&mut self, x: f32, y: f32) -> Task<Message> {
        if self.pan_in_flight {
            self.pending_pan = Some((x, y));
            return Task::none();
        }
        self.pan_in_flight = true;
        pan_task(self.owner.clone(), self.client, x, y)
    }

    pub(super) fn session_command(&mut self, method: &'static str, params: Value) -> Task<Message> {
        if self.state.is_none() || self.busy {
            return Task::none();
        }
        self.busy = true;
        self.status = format!("Running {method}…");
        session_task(self.owner.clone(), self.client, method, params)
    }

    /// Fold in the one `workspace.set` a just-started or just-ended draft still needs, whatever
    /// route opened or closed it. `Message::SetMode` already asks the session itself and clears
    /// this before returning, so it is never doubled.
    pub(super) fn sync_mode(&mut self, task: Task<Message>) -> Task<Message> {
        let Some(target) = self.mode_sync.take() else {
            return task;
        };
        if target == self.session.workspace.mode {
            return task;
        }
        Task::batch([
            task,
            workspace_task(self.owner.clone(), self.client, json!({ "mode": target })),
        ])
    }

    /// What the status bar says on entering a canvas mode that samples the photograph: the mode's
    /// own declared title and the one thing it is waiting for. The title comes from the
    /// descriptor, so no module is named here.
    pub(super) fn canvas_mode_hint(&self) -> Option<String> {
        let mode = &self.session.workspace.mode;
        tools::canvas_pick(&self.modules, mode)?;
        let title = tools::module_of(&self.modules, mode)?
            .canvas
            .as_ref()?
            .title();
        Some(format!("{title} · click the photograph to pick from it"))
    }

    pub(super) fn gallery_page(&self) -> Option<usize> {
        self.developer
            .then_some(self.session.workspace.component_gallery)
            .flatten()
    }
}
