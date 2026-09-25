//! Per-client view state the owner holds in the session: zoom and pan, the side panels, thirds,
//! the canvas mode, the developer gallery page, and the local focus, menu and window facts.
use super::{
    Editor,
    evidence::Settle,
    gesture::Starting,
    message::{CropMessage, Message, Panel, ViewMessage},
    tasks::{pan_task, session_task, workspace_task},
};
use crate::{state::tools, view};
use iced::{Task, widget::operation};
use serde_json::{Map, Value, json};

impl Editor {
    /// One per-client view-state message.
    pub(super) fn view_update(&mut self, message: ViewMessage) -> Task<Message> {
        match message {
            ViewMessage::GalleryPreview => return Task::none(),
            ViewMessage::Gallery(page) => {
                if !self.developer
                    || page.is_some_and(|page| view::gallery_page_info(page).is_none())
                {
                    return Task::none();
                }
                if page.is_some()
                    && (self.busy
                        || self.gesture_refusal(Starting::Gallery).is_some()
                        || self.compare_return.is_some())
                {
                    self.status = "Finish the current operation before opening Components".into();
                    return Task::none();
                }
                self.palette_open = false;
                self.menu = None;
                return workspace_task(
                    self.owner.clone(),
                    self.client,
                    json!({"component_gallery": page}),
                );
            }
            ViewMessage::CopyStatus => {
                // While the status still reads an import's summary, Copy copies its whole report.
                let text = match &self.status_copy {
                    Some((line, detail)) if *line == self.status => detail.clone(),
                    _ => self.status.clone(),
                };
                return iced::clipboard::write(text);
            }
            ViewMessage::SessionUpdated(result) => {
                self.busy = false;
                match result {
                    Ok(session) => {
                        self.adopt(session);
                        self.status = "View updated".into();
                    }
                    Err(error) => self.status = error,
                }
                self.settle_step(Settle::Session);
            }
            ViewMessage::WorkspaceUpdated(result) => {
                match result {
                    Ok(session) => {
                        self.adopt(session);
                        // A canvas mode that picks from the photograph says so while it waits,
                        // whichever route entered it: the strip, its letter, the palette or a
                        // script all arrive here through the same `workspace.set`.
                        if let Some(hint) = self.canvas_mode_hint() {
                            self.status = hint;
                        }
                    }
                    Err(error) => self.status = error,
                }
                self.settle_step(Settle::Session);
            }
            ViewMessage::PanSynced(result) => {
                self.pan_in_flight = false;
                match result {
                    Ok(session) => self.adopt(session),
                    Err(error) => self.status = error,
                }
                if let Some((x, y)) = self.pending_pan.take() {
                    return self.pan(x, y);
                }
                self.settle_step(Settle::Pan);
            }
            ViewMessage::Resized(width, height) => self.window = (width, height),
            ViewMessage::TogglePanel(panel) => {
                let open = match panel {
                    Panel::State => self.session.workspace.state_panel,
                    Panel::Tools => self.session.workspace.tools_panel,
                };
                let mut params = Map::new();
                params.insert(panel.field().into(), Value::from(!open));
                return workspace_task(self.owner.clone(), self.client, Value::Object(params));
            }
            ViewMessage::ToggleThirds => {
                return workspace_task(
                    self.owner.clone(),
                    self.client,
                    json!({"thirds": !self.session.workspace.thirds}),
                );
            }
            ViewMessage::SetMode(mode) => {
                // A draft is never discarded implicitly: leaving the crop mode or Mask mode with a
                // draft open asks for Apply or Cancel, and a slider gesture is finished deliberately
                // rather than replaced by a mode that would need the client's one draft.
                if mode != self.session.workspace.mode
                    && let Some(reason) = self.gesture_refusal(Starting::Mode)
                {
                    self.status = reason;
                    return Task::none();
                }
                let opens_draft = tools::crop_frame(&self.modules)
                    .is_some_and(|frame| frame.module.id == mode)
                    && self.crop().is_none()
                    && self.crop_pending().is_none();
                self.mode_sync = None;
                if opens_draft {
                    // The crop mode is the draft's: a start asks the session to enter it, through
                    // `sync_mode`, only once the draft has started, so a refused start sends
                    // nothing and leaves the session's mode where it was.
                    return self.crop_update(CropMessage::Start);
                }
                // This arm asks the session to follow the mode explicitly, having cleared the
                // generic catch-up in `sync_mode`, so the same field is never asked for twice.
                return workspace_task(self.owner.clone(), self.client, json!({ "mode": mode }));
            }
            ViewMessage::OpenMenu(target) => self.menu = Some(target),
            ViewMessage::CloseMenu => self.menu = None,
            ViewMessage::FocusNext => return operation::focus_next(),
            ViewMessage::FocusPrevious => return operation::focus_previous(),
            ViewMessage::Zoom(value) => self.zoom = value,
            ViewMessage::Panned(x, y) => return self.pan(x, y),
            ViewMessage::Fit => {
                self.zoom = "Fit".into();
                return self.session_command("view.set", json!({"zoom":{"mode":"fit"}}));
            }
            ViewMessage::HundredPercent => {
                self.zoom = "100".into();
                return self
                    .session_command("view.set", json!({"zoom":{"mode":"percent","value":100.0}}));
            }
            ViewMessage::ApplyZoom => {
                let Ok(value) = self.zoom.parse::<f32>() else {
                    self.status = "Zoom must be Fit or a percentage from 10 to 1600".into();
                    return Task::none();
                };
                return self
                    .session_command("view.set", json!({"zoom":{"mode":"percent","value":value}}));
            }
            ViewMessage::ScaleFactor(scale) => {
                if scale.is_finite() && scale > 0.0 {
                    self.scale_factor = scale;
                }
            }
        }
        Task::none()
    }

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
    /// route opened or closed it. `ViewMessage::SetMode` already asks the session itself and clears
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
