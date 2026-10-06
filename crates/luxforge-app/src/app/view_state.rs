//! Per-client view state the owner holds in the session: zoom and pan, the side panels, thirds and
//! the canvas mode; and this desktop's own: the developer gallery page and the local focus, menu
//! and window facts.
use super::{
    Editor,
    gesture::Starting,
    message::{Message, crop::CropMessage, view::ViewMessage},
    outcome::Outcome,
    tasks::{pan_task, session_task, workspace_task},
};
use crate::app::Before;
use crate::state::palette::Panel;
use crate::{
    state::{title, tools},
    view, window_frame,
};
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
                    && let Some(reason) = self.gesture_refusal(Starting::Gallery).or_else(|| {
                        self.document
                            .compare_return
                            .is_some()
                            .then(|| "Release Compare before opening Components".to_owned())
                    })
                {
                    self.status.text = reason;
                    return Task::none();
                }
                // The page is this desktop's own view state, so opening, turning and closing the
                // board is local and immediate: nothing is sent to the owner.
                self.palette.open = false;
                self.view_state.menu = None;
                self.view_state.gallery = page;
            }
            ViewMessage::CopyStatus => {
                // While the status still reads an import's summary, Copy copies its whole report.
                let text = match &self.status.copy {
                    Some((line, detail)) if *line == self.status.text => detail.clone(),
                    _ => self.status.text.clone(),
                };
                return iced::clipboard::write(text);
            }
            ViewMessage::SessionUpdated(result) => {
                self.busy = false;
                match result {
                    Ok(session) => {
                        self.adopt(session);
                        self.status.text = "View updated".into();
                    }
                    Err(error) => self.status.text = error,
                }
                self.outcome(Outcome::SessionAnswered);
            }
            ViewMessage::WorkspaceUpdated(result) => {
                let before = super::remembered::remembered_workspace(&self.session.workspace);
                match result {
                    Ok(session) => {
                        self.adopt(session);
                        // A canvas mode that picks from the photograph says so while it waits,
                        // whichever route entered it: the strip, its letter, the palette or a
                        // script all arrive here through the same `workspace.set`.
                        if let Some(hint) = self.canvas_mode_hint() {
                            self.status.text = hint;
                        }
                    }
                    Err(error) => self.status.text = error,
                }
                self.outcome(Outcome::SessionAnswered);
                return self.remember_workspace(before);
            }
            ViewMessage::PanSynced(result) => {
                self.view_state.pan.answered();
                match result {
                    Ok(session) => self.adopt(session),
                    Err(error) => self.status.text = error,
                }
                if let Some(&(x, y)) = self.view_state.pan.pending() {
                    return self.pan(x, y);
                }
                self.outcome(Outcome::PanAnswered);
            }
            ViewMessage::Resized(width, height) => {
                self.view_state.window = (width, height);
                // Entering or leaving fullscreen resizes the window, and nothing else reports it.
                if window_frame::INTEGRATED_TITLE_BAR {
                    return iced::window::oldest()
                        .and_then(iced::window::mode)
                        .map(|mode| {
                            Message::View(ViewMessage::Fullscreen(
                                mode == iced::window::Mode::Fullscreen,
                            ))
                        });
                }
            }
            ViewMessage::Fullscreen(fullscreen) => self.view_state.fullscreen = fullscreen,
            ViewMessage::Placed { report, on_main } => return self.window_placed(report, on_main),
            ViewMessage::ClosingFrame(report) => return self.closing_frame(report),
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
            ViewMessage::ToggleInformation => {
                return workspace_task(
                    self.owner.clone(),
                    self.client,
                    json!({"information": !self.session.workspace.information}),
                );
            }
            ViewMessage::SetMode(mode) => {
                // A draft is never discarded implicitly: leaving the crop mode or Mask mode with a
                // draft open asks for Apply or Cancel, and a slider gesture is finished deliberately
                // rather than replaced by a mode that would need the client's one draft.
                if mode != self.session.workspace.mode
                    && let Some(reason) = self.gesture_refusal(Starting::Mode)
                {
                    self.status.text = reason;
                    return Task::none();
                }
                // A module pick entered while the sections are bound to a mask stays bound to it,
                // exactly as the resolved picker the person pressed was: Basic's, on that mask.
                self.mask_panel.pick_on_mask = tools::module_of(&self.modules, &mode).is_some()
                    && tools::canvas_pick(&self.modules, &mode).is_some()
                    && self.section_target().is_some();
                let opens_draft = tools::crop_frame(&self.modules)
                    .is_some_and(|frame| frame.module.id == mode)
                    && self.crop_gesture().is_none();
                self.sync.mode = None;
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
            ViewMessage::OpenMenu(target) => self.view_state.menu = Some(target),
            ViewMessage::OpenControlMenu {
                action,
                parameter,
                preset,
            } => {
                self.view_state.menu =
                    Some(crate::state::MenuTarget::control(action, parameter, preset))
            }
            ViewMessage::CloseMenu => self.view_state.menu = None,
            ViewMessage::FocusNext => return operation::focus_next(),
            ViewMessage::FocusPrevious => return operation::focus_previous(),
            ViewMessage::Zoom(value) => self.view_state.zoom = value,
            // AppKit reports the pointer in the system's points; the layout is in logical pixels,
            // which are the interface size's multiple of them.
            ViewMessage::Pinch(input) => {
                let scale = f64::from(self.view_state.interface_scale());
                return self.pinch(luxforge_input::Pinch {
                    x: input.x / scale,
                    y: input.y / scale,
                    ..input
                });
            }
            #[cfg(target_os = "macos")]
            ViewMessage::PinchPending => {
                if let Some(input) = super::waker::take_pinch() {
                    return self.pinch(input);
                }
            }
            #[cfg(target_os = "macos")]
            ViewMessage::PinchInstalled(result) => match result {
                Ok(()) => self.event("trackpad_input_ready", || json!({"platform": "macos"})),
                Err(reason) => {
                    self.event("trackpad_input_failed", || json!({"reason": reason}));
                    self.status.text = reason;
                }
            },
            ViewMessage::Panned(x, y) => return self.pan(x, y),
            ViewMessage::Fit => {
                self.view_state.zoom = "Fit".into();
                self.view_state.zoom_editing = false;
                return self.session_command("view.set", json!({"zoom":{"mode":"fit"}}));
            }
            ViewMessage::HundredPercent => return self.zoom_to(100.0),
            ViewMessage::ZoomTo(value) => return self.zoom_to(value),
            ViewMessage::ZoomStep(step) => {
                if !self.workspace.title.can_view {
                    return Task::none();
                }
                // Shown even with no stop left that way, so the end of the rail is seen.
                self.view_state.zoom_reveal = self.view_state.zoom_reveal.wrapping_add(1);
                let target = self
                    .workspace
                    .title
                    .zoom_effective
                    .and_then(|percent| title::step_stop(&title::ZOOM_STOPS, percent, step));
                if let Some(value) = target {
                    return self.zoom_to(value);
                }
            }
            ViewMessage::ApplyZoom => {
                let Ok(value) = self.view_state.zoom.parse::<f32>() else {
                    self.status.text = "Zoom must be Fit or a percentage from 10 to 1600".into();
                    return Task::none();
                };
                self.view_state.zoom_editing = false;
                return self
                    .session_command("view.set", json!({"zoom":{"mode":"percent","value":value}}));
            }
            ViewMessage::EditZoom => {
                // The field starts from what the segment showed, without its percent sign.
                self.view_state.zoom = self
                    .workspace
                    .title
                    .zoom_percent
                    .trim_end_matches('%')
                    .to_owned();
                self.view_state.zoom_editing = true;
                return Task::batch([
                    operation::focus(view::title_bar::ZOOM_FIELD),
                    operation::select_all(view::title_bar::ZOOM_FIELD),
                ]);
            }
            ViewMessage::DragWindow => {
                return iced::window::oldest().and_then(iced::window::drag);
            }
            // The system's factor alone: Iced's answer leaves out the application's scale factor,
            // which the interface size sets.
            ViewMessage::ScaleFactor(scale) => self.view_state.set_system_scale_factor(scale),
        }
        Task::none()
    }

    /// Ask the session for a percentage zoom, as the 100% segment and a zoom stop do.
    fn zoom_to(&mut self, value: f32) -> Task<Message> {
        self.view_state.zoom = title::percent_text(value).trim_end_matches('%').to_owned();
        self.view_state.zoom_editing = false;
        self.session_command("view.set", json!({"zoom":{"mode":"percent","value":value}}))
    }

    /// Pan is session state like zoom, but scroll events arrive faster than round trips complete:
    /// keep one request in flight and only the newest pending position.
    pub(super) fn pan(&mut self, x: f32, y: f32) -> Task<Message> {
        if (x, y) != self.view_state.local_pan {
            self.view_state.local_pan = (x, y);
            self.note_view_motion();
        }
        self.view_state.pan.offer((x, y));
        match self.view_state.pan.start() {
            Some((x, y)) => pan_task(self.owner.clone(), self.client, x, y),
            None => Task::none(),
        }
    }

    pub(super) fn session_command(&mut self, method: &'static str, params: Value) -> Task<Message> {
        if self.document.state.is_none() || self.busy {
            return Task::none();
        }
        self.busy = true;
        self.status.text = format!("Running {method}…");
        session_task(self.owner.clone(), self.client, method, params)
    }

    /// Fold in the one `workspace.set` a just-started or just-ended draft still needs, whatever
    /// route opened or closed it. `ViewMessage::SetMode` already asks the session itself and clears
    /// this before returning, so it is never doubled.
    pub(super) fn sync_mode(&mut self) -> Task<Message> {
        let Some(target) = self.sync.mode.take() else {
            return Task::none();
        };
        if target == self.session.workspace.mode {
            return Task::none();
        }
        workspace_task(self.owner.clone(), self.client, json!({ "mode": target }))
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
        self.developer.then_some(self.view_state.gallery).flatten()
    }
}

/// AppKit monitor registration must run on the window runtime's main thread.
pub(super) fn install_trackpad() -> Task<Message> {
    #[cfg(target_os = "macos")]
    {
        iced::window::oldest()
            .and_then(|id| {
                iced::window::run(id, |window| {
                    luxforge_input::install_pinch_handler(
                        window,
                        std::sync::Arc::new(super::waker::post_pinch),
                    )
                })
            })
            .map(|result| Message::View(ViewMessage::PinchInstalled(result)))
    }
    #[cfg(not(target_os = "macos"))]
    Task::none()
}

pub(super) fn subscription(_editor: &Editor) -> iced::Subscription<Message> {
    #[cfg(target_os = "macos")]
    return super::waker::pinch_subscription();
    #[cfg(not(target_os = "macos"))]
    iced::Subscription::none()
}

/// After every message: whatever route moved the zoom, the window, the display scale, a side panel
/// or the pan — a button, the field, a script, a resize or an API client's `view.set` reaching us
/// through an adopted session — is view motion, unless the message already noted it. The typed
/// zoom field closes on any zoom change, so the segment shows the zoom it now holds.
pub(super) fn after_message(editor: &mut Editor, before: &Before) -> Task<Message> {
    let zoomed = editor.session.preview.view.zoom != before.zoom;
    if (zoomed || editor.view_geometry() != before.geometry)
        && editor.view_plan.epoch == before.view_epoch
    {
        editor.note_view_motion();
    }
    if zoomed {
        editor.view_state.zoom_editing = false;
    }
    Task::none()
}
