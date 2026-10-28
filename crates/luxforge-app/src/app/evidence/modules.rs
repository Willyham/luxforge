//! The `modules` evidence step ([`ModulesStep`]): each gesture on the person's module layout, and
//! each external-module gesture of Settings › Modules, sent through the message its control sends,
//! captured once every layout and lifecycle write outstanding has answered, any package being read
//! has been, and the owner's `module.layout.read`, `module.list` and `module.package.list` have
//! been read beside the frame, and what a captured frame records of the layout the workspace draws
//! by.
use super::Settle;
use crate::app::{
    Editor,
    message::{
        Message,
        evidence::EvidenceMessage,
        modules::ModulesMessage,
        settings::{ModulesKey, ModulesMessage as TabMessage, SettingsMessage},
        view::ViewMessage,
    },
    tasks::{call, owner_task},
};
use crate::state::{
    MenuTarget,
    external_modules::ReviewState,
    module_layout::Move,
    module_settings::{self, RowKey},
    settings::SettingsTab,
};
use iced::Task;
use luxforge_core::Toolset;
use luxforge_evidence::ModulesStep;
use serde_json::{Value, json};

/// What a running `modules` step waits for: the layout writes and reads it started to answer, then
/// the owner's reads it is correlated with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModulesWait {
    Writes,
    Reading,
}

fn toolset(name: &str) -> Option<Toolset> {
    match name {
        "adjustments" => Some(Toolset::Adjustments),
        "tools" => Some(Toolset::Tools),
        _ => None,
    }
}

fn workspace_move(to: &str) -> Option<Move> {
    match to {
        "up" => Some(Move::Up),
        "down" => Some(Move::Down),
        "top" => Some(Move::Top),
        "bottom" => Some(Move::Bottom),
        _ => None,
    }
}

fn tab_move(to: &str) -> Option<module_settings::Move> {
    match to {
        "up" => Some(module_settings::Move::Up),
        "down" => Some(module_settings::Move::Down),
        "top" => Some(module_settings::Move::Top),
        "bottom" => Some(module_settings::Move::Bottom),
        _ => None,
    }
}

impl Editor {
    /// Send one module layout gesture. A gesture that should write and sends nothing — a move that
    /// changes nothing, a locked module turned off — fails the step with the status bar's words.
    pub(super) fn modules_step(&mut self, step: ModulesStep) -> Task<Message> {
        let writes = matches!(
            step,
            ModulesStep::Move(_)
                | ModulesStep::Enabled(_)
                | ModulesStep::TabMove(_)
                | ModulesStep::ResetAll
                | ModulesStep::ReviewInstall
                | ModulesStep::ConfirmUninstall
        );
        let before = self.layout_writes();
        let tab = |message: TabMessage| Message::Settings(SettingsMessage::Modules(message));
        let task = match step {
            ModulesStep::BandMenu(module) => {
                if !self
                    .workspace
                    .tools
                    .sections
                    .iter()
                    .chain(&self.workspace.tools.developer)
                    .any(|section| section.module_id == module)
                {
                    return self.fail_step(format!("the tools panel draws no band {module}"));
                }
                self.update(Message::View(ViewMessage::OpenMenu(MenuTarget::Band(
                    module,
                ))))
            }
            ModulesStep::StripMenu => {
                self.update(Message::View(ViewMessage::OpenMenu(MenuTarget::Strip)))
            }
            ModulesStep::CloseMenu => self.update(Message::View(ViewMessage::CloseMenu)),
            ModulesStep::Move(step) => {
                let Some(to) = workspace_move(&step.to) else {
                    return self.fail_step(format!("no move {}", step.to));
                };
                self.update(Message::Modules(ModulesMessage::Move {
                    toolset: Toolset::Adjustments,
                    module: step.module,
                    to,
                }))
            }
            ModulesStep::Enabled(step) => {
                self.update(Message::Modules(ModulesMessage::SetEnabled {
                    module: step.module,
                    enabled: step.on,
                }))
            }
            ModulesStep::Customize(step) => {
                let Some(toolset) = toolset(&step.toolset) else {
                    return self.fail_step(format!("no toolset {}", step.toolset));
                };
                self.update(Message::Modules(ModulesMessage::Customize {
                    toolset,
                    module: step.module,
                }))
            }
            ModulesStep::TabMove(step) => {
                if self.settings.open != Some(SettingsTab::Modules) {
                    return self.fail_step("Settings › Modules is not open");
                }
                let (Some(toolset), Some(how)) = (toolset(&step.toolset), tab_move(&step.to))
                else {
                    return self.fail_step(format!("no move {} in {}", step.to, step.toolset));
                };
                let row = RowKey {
                    toolset,
                    module: step.module,
                };
                if !self.settings.modules.rows().contains(&row) {
                    return self.fail_step(format!(
                        "Settings › Modules lists no {} row in {}",
                        row.module, step.toolset
                    ));
                }
                let selected = self.update(Message::Settings(SettingsMessage::Modules(
                    TabMessage::Select(row),
                )));
                let moved = self.update(Message::Settings(SettingsMessage::Modules(
                    TabMessage::Key(ModulesKey::Move(how)),
                )));
                Task::batch([selected, moved])
            }
            ModulesStep::ResetAll => {
                if self.settings.open != Some(SettingsTab::Modules) {
                    return self.fail_step("Settings › Modules is not open");
                }
                self.update(tab(TabMessage::ResetAll))
            }
            // Install module… with the path picked: the tab opens as the gesture opens it, and the
            // path goes where the system file picker's answer goes.
            ModulesStep::Install(path) => {
                let opened = if self.settings.open == Some(SettingsTab::Modules) {
                    Task::none()
                } else {
                    self.update(Message::Settings(SettingsMessage::Open(
                        SettingsTab::Modules,
                    )))
                };
                let picked = self.update(tab(TabMessage::InstallPicked(Some(path.into()))));
                if self.settings.modules.review.is_none() {
                    return Task::batch([
                        opened,
                        picked,
                        self.fail_step("Install module… opened no review sheet"),
                    ]);
                }
                Task::batch([opened, picked])
            }
            ModulesStep::ReviewInstall => self.update(tab(TabMessage::ReviewInstall)),
            ModulesStep::Uninstall(module) => {
                if self.settings.open != Some(SettingsTab::Modules) {
                    return self.fail_step("Settings › Modules is not open");
                }
                let task = self.update(tab(TabMessage::Uninstall(module.clone())));
                if self.settings.modules.uninstall.is_none() {
                    return Task::batch([
                        task,
                        self.fail_step(format!("{module} offers no Uninstall…")),
                    ]);
                }
                task
            }
            ModulesStep::ConfirmUninstall => self.update(tab(TabMessage::UninstallConfirm)),
            ModulesStep::Dismiss => {
                if !self.settings.modules.dialog() {
                    return self.fail_step("no review sheet or confirmation is open");
                }
                self.update(tab(TabMessage::Dismiss))
            }
            ModulesStep::Relaunch => {
                let task = self.update(tab(TabMessage::Relaunch));
                if self.settings.modules.relaunch.is_none() {
                    return Task::batch([task, self.fail_step(self.status.text.clone())]);
                }
                task
            }
        };
        if writes && self.layout_writes() == before {
            return Task::batch([task, self.fail_step(self.status.text.clone())]);
        }
        self.await_modules();
        task
    }

    /// The layout and lifecycle writes this desktop has sent, the workspace's and the Modules
    /// tab's, counted so a step can tell whether its gesture sent one.
    fn layout_writes(&self) -> (bool, usize, bool) {
        let tab = &self.settings.modules;
        (
            self.layout.pending,
            usize::from(tab.writing.is_some()) + tab.waiting.len(),
            tab.package_write.is_some(),
        )
    }

    /// The running step waits for the layout writes and reads in flight, then the owner's reads.
    pub(super) fn await_modules(&mut self) {
        self.await_step(Settle::Modules);
        if let Some(evidence) = &mut self.evidence {
            evidence.modules_wait = Some(ModulesWait::Writes);
        }
    }

    /// Once nothing about the layout is in flight, read the owner's layout and listing for the
    /// running step's record. Checked after every message.
    pub(super) fn settle_modules(&mut self) -> Task<Message> {
        let waiting = self
            .evidence
            .as_ref()
            .filter(|evidence| evidence.awaiting == Some(Settle::Modules))
            .and_then(|evidence| evidence.modules_wait);
        if waiting != Some(ModulesWait::Writes) {
            return Task::none();
        }
        let tab = &self.settings.modules;
        let opening = self.settings.open == Some(SettingsTab::Modules) && tab.read.is_none();
        let inspecting = tab
            .review
            .as_ref()
            .is_some_and(|review| review.state == ReviewState::Inspecting);
        if self.layout.pending
            || !tab.idle()
            || tab.reading
            || opening
            || inspecting
            || !self.modules_ready
        {
            return Task::none();
        }
        if let Some(evidence) = &mut self.evidence {
            evidence.modules_wait = Some(ModulesWait::Reading);
        }
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let (layout, _) = call(&owner, client, "module.layout.read", json!({}))?;
                let (listed, _) = call(&owner, client, "module.list", json!({}))?;
                let (packages, _) = call(&owner, client, "module.package.list", json!({}))?;
                Ok(json!({"layout": layout, "modules": listed["modules"], "packages": packages}))
            },
            |result| Message::Evidence(EvidenceMessage::ModulesRead(result)),
        )
    }

    /// The owner's reads answered: the step's record keeps them, each module as its id, home,
    /// switch and placements, and the frame is captured.
    pub(super) fn modules_read(&mut self, result: Result<Value, String>) {
        if self
            .evidence
            .as_ref()
            .and_then(|evidence| evidence.modules_wait)
            != Some(ModulesWait::Reading)
        {
            return;
        }
        if let Some(evidence) = &mut self.evidence {
            evidence.modules_wait = None;
        }
        match result {
            Ok(read) => {
                let modules: Vec<Value> = read["modules"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|module| {
                        json!({"id": module["id"], "toolset": module["toolset"],
                               "enabled": module["enabled"], "locked": module["locked"],
                               "placements": module["placements"]})
                    })
                    .collect();
                self.note_step(json!({"owner": {"module_layout_read": read["layout"],
                                                 "module_list": modules,
                                                 "module_package_list": read["packages"]}}));
                self.settle_step(Settle::Modules, "module_layout_read");
            }
            Err(reason) => {
                let _ = self.fail_step(format!("reading the module layout failed: {reason}"));
            }
        }
    }
}
