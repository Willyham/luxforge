//! The person's module layout ([module manager](../../../../docs/design/module-manager.md#desktop)):
//! reading it with discovery and again on another client's `module.layout` event, and the writes a
//! band's menu, the mode strip's ⋯ menu and the palette send.
//!
//! Every write is one owner method — `module.enable`, `module.disable` or `module.order.set` — sent
//! off the update loop, and the layout it answers is adopted as authoritative, so the panel, the
//! strip, the palette and the keyboard follow what the owner holds. A move computes the toolset's
//! whole order here and sends it. Nothing here touches a photograph: no edit, history entry, render
//! or export follows from a layout change, and the status bar says so.
use super::{
    Editor,
    message::{
        Message,
        modules::{ModulesMessage, Written},
        settings::SettingsMessage,
        view::ViewMessage,
    },
    tasks::{layout_read_task, layout_write_task},
};
use crate::state::{
    MenuTarget,
    module_layout::{self, Move},
    settings::SettingsTab,
    tools::module_of,
};
use iced::Task;
use luxforge_core::{
    ModuleDescriptor, POINTER_MODE, Toolset,
    module_layout::{LAYOUT_EVENT, LayoutView},
};
use serde_json::json;
use std::collections::BTreeMap;

impl Editor {
    /// One layout gesture or owner answer.
    pub(super) fn modules_update(&mut self, message: ModulesMessage) -> Task<Message> {
        match message {
            ModulesMessage::Discovered(result) => match result {
                Ok(discovery) => {
                    let modules: Vec<ModuleDescriptor> = discovery
                        .modules
                        .iter()
                        .map(|listed| listed.descriptor.clone())
                        .collect();
                    self.layout.adopt_listing(&discovery.modules, &modules);
                    self.layout.adopt_view(discovery.layout, &modules);
                    self.modules_loaded(Ok(modules));
                    self.layout_event("read");
                }
                Err(error) => self.modules_loaded(Err(error)),
            },
            ModulesMessage::Read(result) => match result {
                Ok(view) => return self.adopt_layout(view, "read"),
                Err(error) => self.status.text = format!("Reading your modules failed: {error}"),
            },
            ModulesMessage::SetEnabled { module, enabled } => {
                self.view_state.menu = None;
                return self.set_module_enabled(&module, enabled);
            }
            ModulesMessage::Move {
                toolset,
                module,
                to,
            } => {
                self.view_state.menu = None;
                return self.move_module(toolset, &module, to);
            }
            ModulesMessage::MoveOpenBand(to) => {
                // The open band's menu stands in for the band's keyboard focus: the menu stays open
                // so the band can be moved again.
                if let Some(MenuTarget::Band(module)) = self.view_state.menu.clone() {
                    return self.move_module(Toolset::Adjustments, &module, to);
                }
            }
            ModulesMessage::Written(result) => {
                self.layout.pending = false;
                match result {
                    Ok(written) => {
                        let Written {
                            layout,
                            request,
                            status,
                        } = *written;
                        // The event this request announced is read and skipped by the next poll.
                        self.read_back(request);
                        let task = self.adopt_layout(layout, "written");
                        self.status.text = status;
                        return task;
                    }
                    Err(error) => {
                        self.status.text = format!("Changing your modules failed: {error}");
                        self.event("module_layout_failed", || json!({ "message": error }));
                    }
                }
            }
            ModulesMessage::CopyBand(module) => {
                self.view_state.menu = None;
                let Some(descriptor) = module_of(&self.modules, &module) else {
                    return Task::none();
                };
                let shown = self.shown_sections(&module);
                let shown: Vec<&str> = shown.iter().map(String::as_str).collect();
                let requests = module_layout::band_requests(&self.layout, descriptor, &shown);
                self.status.text = "Copied the module layout requests".to_owned();
                return iced::clipboard::write(
                    serde_json::to_string_pretty(&requests).unwrap_or_default(),
                );
            }
            ModulesMessage::Customize { toolset, module } => {
                self.view_state.menu = None;
                return self.customize_modules(toolset, module);
            }
        }
        Task::none()
    }

    /// Read the layout again, after another client changed it.
    pub(crate) fn read_layout(&self) -> Task<Message> {
        layout_read_task(self.owner.clone(), self.client)
    }

    /// Open where the person arranges `toolset`'s modules, with `module` selected when the gesture
    /// came from one. Every Customize gesture — a band's Customize panel…, the strip's Customize
    /// tools…, the footer's Customize… and the palette's — comes here. For now it opens the
    /// Settings sheet.
    fn customize_modules(&mut self, toolset: Toolset, module: Option<String>) -> Task<Message> {
        self.event(
            "module_layout_customize",
            || json!({ "toolset": toolset.as_str(), "module": module }),
        );
        self.dispatch(Message::Settings(SettingsMessage::Open(
            SettingsTab::General,
        )))
    }

    /// Adopt the layout the owner answered, and leave a canvas mode whose module is now off.
    fn adopt_layout(&mut self, view: LayoutView, how: &str) -> Task<Message> {
        self.layout.adopt_view(view, &self.modules);
        self.layout_event(how);
        let mode = &self.session.workspace.mode;
        if module_of(&self.modules, mode).is_some() && !self.layout.enabled(mode) {
            return self.dispatch(Message::View(ViewMessage::SetMode(POINTER_MODE.into())));
        }
        Task::none()
    }

    /// Record the layout the desktop now draws by, for evidence to correlate with
    /// `module.layout.read`.
    fn layout_event(&self, how: &str) {
        self.event(
            LAYOUT_EVENT,
            || json!({ "how": how, "layout": self.layout.view() }),
        );
    }

    /// `module.enable` or `module.disable` for `module`. A locked module is refused here with its
    /// reason, as the owner would refuse it; a write already in flight is waited for.
    fn set_module_enabled(&mut self, module: &str, enabled: bool) -> Task<Message> {
        if self.layout.pending {
            return Task::none();
        }
        let Some(descriptor) = module_of(&self.modules, module) else {
            return Task::none();
        };
        if !enabled && let Some(reason) = self.layout.lock(module) {
            self.status.text = format!("{} is always on: {reason}", descriptor.title);
            return Task::none();
        }
        if self.layout.enabled(module) == enabled {
            return Task::none();
        }
        let status = module_layout::switched_status(&descriptor.title, enabled);
        let method = if enabled {
            "module.enable"
        } else {
            "module.disable"
        };
        self.start_layout_write(method, json!({ "module": module }), status)
    }

    /// Send one layout write, holding every other until it answers.
    fn start_layout_write(
        &mut self,
        method: &'static str,
        params: serde_json::Value,
        status: String,
    ) -> Task<Message> {
        self.layout.pending = true;
        self.event(
            "module_layout_write",
            || json!({ "method": method, "params": params }),
        );
        #[cfg(test)]
        self.layout_started
            .push((method.to_owned(), params.clone(), status.clone()));
        layout_write_task(self.owner.clone(), self.client, method, params, status)
    }

    /// `module.order.set` with `toolset`'s whole order after moving `module`, relative to the
    /// placements drawn now. A move that changes nothing sends nothing.
    fn move_module(&mut self, toolset: Toolset, module: &str, to: Move) -> Task<Message> {
        if self.layout.pending {
            return Task::none();
        }
        let Some(order) = self.layout.order(toolset).map(<[String]>::to_vec) else {
            return Task::none();
        };
        let shown = match toolset {
            Toolset::Tools => self
                .workspace
                .canvas
                .modes
                .iter()
                .map(|mode| mode.id.clone())
                .filter(|id| order.contains(id))
                .collect(),
            _ => self.shown_sections(module),
        };
        let shown: Vec<&str> = shown.iter().map(String::as_str).collect();
        let Some(order) = module_layout::moved(&order, &shown, module, to) else {
            return Task::none();
        };
        let titles: BTreeMap<&str, &str> = self
            .modules
            .iter()
            .map(|module| (module.id.as_str(), module.title.as_str()))
            .collect();
        let place = match toolset {
            Toolset::Tools => "strip",
            _ => "panel",
        };
        let status = module_layout::moved_status(&titles, &order, &shown, module, place);
        self.start_layout_write(
            "module.order.set",
            json!({ "toolset": toolset.as_str(), "order": order }),
            status,
        )
    }

    /// The sections drawn now in the group `module`'s band is in — the developer group for a
    /// developer module — in drawn order.
    fn shown_sections(&self, module: &str) -> Vec<String> {
        let tools = &self.workspace.tools;
        let group = if tools
            .developer
            .iter()
            .any(|section| section.module_id == module)
        {
            &tools.developer
        } else {
            &tools.sections
        };
        group
            .iter()
            .map(|section| section.module_id.clone())
            .collect()
    }
}
