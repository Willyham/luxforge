//! Settings › Modules' external modules ([design](../../../../docs/design/module-manager.md#desktop),
//! [boards E and F](../../../../docs/design/module-manager/screens.md)): Install module… through
//! the system file picker and the review sheet `module.package.inspect` answers, `module.install`
//! from the sheet, Uninstall…'s confirmation and `module.uninstall`, Load from folder… and Remove
//! in developer mode (`module.folder.load`, `module.folder.remove`), Copy error, and Relaunch,
//! which applies the pending changes by starting the desktop again once the catalog has closed.
//!
//! Every package is read, checked and copied on the owner's index lane; here each call runs off
//! the update loop ([`owner_task`]), one lifecycle write at a time, and its answer is shown at once
//! and read back with the tab's next read. An evidence run never opens a native picker: its
//! `modules` step hands the package's path to [`ModulesMessage::InstallPicked`].
use super::{
    Editor,
    message::{
        Message,
        settings::{ModulesMessage, SettingsMessage},
    },
    relaunch,
    tasks::{call_detailed, call_own_detailed, owner_task},
};
use crate::state::{
    external_modules::{Inspected, PackageRow, PackageWrite, Review, ReviewState, Uninstall},
    module_settings::ModulesFocus,
    settings::SettingsTab,
};
use iced::Task;
use luxforge_core::module_layout::ModuleOrigin;
use serde_json::{Value, json};
use std::path::PathBuf;

fn modules(message: ModulesMessage) -> Message {
    Message::Settings(SettingsMessage::Modules(message))
}

impl Editor {
    /// One external-module gesture or owner answer of the Modules tab.
    pub(super) fn external_modules_update(&mut self, message: ModulesMessage) -> Task<Message> {
        match message {
            ModulesMessage::Install => return self.install_module(),
            ModulesMessage::InstallPicked(path) => {
                self.settings.modules.picking = false;
                self.view_state.picker_open = false;
                if let Some(path) = path {
                    return self.inspect_package(path);
                }
            }
            ModulesMessage::Inspected { path, result } => self.package_inspected(path, result),
            ModulesMessage::ReviewInstall => {
                let tab = &mut self.settings.modules;
                let Some(review) = tab.review.as_mut().filter(|review| review.installable()) else {
                    return Task::none();
                };
                if tab.package_write.is_some() {
                    return Task::none();
                }
                review.installing = true;
                review.failed = None;
                let path = review.path.clone();
                return self.write_package(PackageWrite::Install(path));
            }
            ModulesMessage::Uninstall(module) => {
                let tab = &mut self.settings.modules;
                let Some(row) = tab.package(&module) else {
                    return Task::none();
                };
                if row.origin != ModuleOrigin::Community
                    || row.installed.is_none()
                    || tab.dialog()
                    || tab.package_write.is_some()
                {
                    return Task::none();
                }
                let title = row.title();
                tab.uninstall = Some(Uninstall {
                    module,
                    title,
                    writing: false,
                    failed: None,
                });
            }
            ModulesMessage::UninstallConfirm => {
                let tab = &mut self.settings.modules;
                if tab.package_write.is_some() {
                    return Task::none();
                }
                let Some(uninstall) = tab.uninstall.as_mut().filter(|ask| !ask.writing) else {
                    return Task::none();
                };
                uninstall.writing = true;
                uninstall.failed = None;
                let module = uninstall.module.clone();
                return self.write_package(PackageWrite::Uninstall(module));
            }
            ModulesMessage::Dismiss => {
                let tab = &mut self.settings.modules;
                // What is in flight lands either way; its dialog waits for the answer.
                if tab.review.as_ref().is_some_and(|review| !review.installing) {
                    tab.review = None;
                } else if tab.uninstall.as_ref().is_some_and(|ask| !ask.writing) {
                    tab.uninstall = None;
                }
            }
            ModulesMessage::Return => {
                let destructive = self.settings.modules.review.as_ref().is_none_or(|review| {
                    crate::state::external_modules::derive_review(review)
                        .confirm
                        .is_none_or(|confirm| confirm.destructive)
                });
                if !destructive {
                    return self.external_modules_update(ModulesMessage::ReviewInstall);
                }
            }
            ModulesMessage::LoadFolder => return self.load_folder(),
            ModulesMessage::FolderPicked(path) => {
                self.settings.modules.picking = false;
                self.view_state.picker_open = false;
                if let Some(path) = path
                    && self.settings.modules.package_write.is_none()
                {
                    return self.write_package(PackageWrite::FolderLoad(path));
                }
            }
            ModulesMessage::RemoveFolder(module) => {
                let developer = self.settings.modules.developer;
                if developer && self.settings.modules.package_write.is_none() {
                    return self.write_package(PackageWrite::FolderRemove(module));
                }
            }
            ModulesMessage::PackageWritten(result) => return self.package_written(result),
            ModulesMessage::CopyError(reason) => {
                self.status.text = "Copied the error".to_owned();
                return iced::clipboard::write(reason);
            }
            ModulesMessage::Relaunch => return self.relaunch_for_modules(),
            _ => {}
        }
        Task::none()
    }

    /// Install module…: open the sheet at Modules when it is not, then the system file picker
    /// for a `.lfmodule` file. Nothing opens while a picker, a dialog or a write is up, or when the
    /// tab has read a store that takes no writes.
    fn install_module(&mut self) -> Task<Message> {
        let opened = if self.settings.open == Some(SettingsTab::Modules) {
            Task::none()
        } else {
            self.settings_update(SettingsMessage::Open(SettingsTab::Modules))
        };
        let tab = &self.settings.modules;
        let refused = tab
            .read
            .as_ref()
            .is_some_and(|read| !read.packages.writable());
        if tab.picking || tab.dialog() || tab.package_write.is_some() || self.view_state.picker_open
        {
            return opened;
        }
        if refused {
            if let Some(problem) = tab.read.as_ref().and_then(|read| read.packages.problem()) {
                self.status.text = problem;
            }
            return opened;
        }
        // An automated run never opens a native dialog: its script hands the path in.
        if self.evidence.is_some() {
            return opened;
        }
        self.settings.modules.picking = true;
        self.view_state.picker_open = true;
        let picked = Task::perform(
            async {
                rfd::AsyncFileDialog::new()
                    .set_title("Install Module")
                    .add_filter("Luxforge module", &["lfmodule"])
                    .pick_file()
                    .await
                    .map(|file| file.path().to_path_buf())
            },
            |path| modules(ModulesMessage::InstallPicked(path)),
        );
        Task::batch([opened, picked])
    }

    /// Load from folder…, in developer mode: the system folder picker.
    fn load_folder(&mut self) -> Task<Message> {
        let tab = &self.settings.modules;
        if !tab.developer
            || !tab.can_install()
            || self.view_state.picker_open
            || self.evidence.is_some()
        {
            return Task::none();
        }
        self.settings.modules.picking = true;
        self.view_state.picker_open = true;
        Task::perform(
            async {
                rfd::AsyncFileDialog::new()
                    .set_title("Load Module from Folder")
                    .pick_folder()
                    .await
                    .map(|folder| folder.path().to_path_buf())
            },
            |path| modules(ModulesMessage::FolderPicked(path)),
        )
    }

    /// The review sheet opens on `path` and `module.package.inspect` reads it, off the update loop.
    fn inspect_package(&mut self, path: PathBuf) -> Task<Message> {
        let tab = &mut self.settings.modules;
        if tab.dialog() || tab.package_write.is_some() {
            return Task::none();
        }
        tab.review = Some(Review::new(path.clone()));
        self.event("module_package_inspect", || json!({ "path": path }));
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                let result = call_detailed(
                    &owner,
                    client,
                    "module.package.inspect",
                    json!({ "path": path }),
                )
                .map_err(|error| {
                    let refusal = error
                        .data
                        .as_ref()
                        .and_then(|data| data["refusal"].as_str())
                        .map(str::to_owned);
                    (error.message, refusal)
                })
                .and_then(|answer| {
                    Inspected::parse(answer)
                        .map(Box::new)
                        .map_err(|reason| (reason, None))
                });
                (path, result)
            },
            |(path, result)| modules(ModulesMessage::Inspected { path, result }),
        )
    }

    /// `module.package.inspect` answered: the sheet shows the package, or the refusal's reason with
    /// no Install. An answer for a sheet no longer open is dropped.
    fn package_inspected(
        &mut self,
        path: PathBuf,
        result: Result<Box<Inspected>, (String, Option<String>)>,
    ) {
        let Some(review) = self
            .settings
            .modules
            .review
            .as_mut()
            .filter(|review| review.path == path && review.state == ReviewState::Inspecting)
        else {
            return;
        };
        let record = match &result {
            Ok(inspected) => json!({"path": path, "module": inspected.manifest.id,
                                    "version": inspected.manifest.version, "hash": inspected.hash}),
            Err((reason, refusal)) => json!({"path": path, "refused": reason, "refusal": refusal}),
        };
        review.state = match result {
            Ok(inspected) => ReviewState::Inspected(inspected),
            Err((reason, refusal)) => ReviewState::Refused { reason, refusal },
        };
        self.event("module_package_inspected", || record);
    }

    /// Send one lifecycle write, off the update loop.
    fn write_package(&mut self, write: PackageWrite) -> Task<Message> {
        let (method, params) = (write.method(), write.params());
        self.settings.modules.package_write = Some(write);
        self.event(
            "module_package_write",
            || json!({ "method": method, "params": params }),
        );
        let (owner, client) = (self.owner.clone(), self.client);
        owner_task(
            move || {
                call_own_detailed(&owner, client, method, params).map_err(|error| error.message)
            },
            |result| modules(ModulesMessage::PackageWritten(result)),
        )
    }

    /// The lifecycle write in flight answered: its answer shows at once (the status bar, the row's
    /// state, the dialog put away) and the tab reads everything again; a refusal is said in the
    /// dialog that sent it, which stays open, or in the detail pane.
    fn package_written(&mut self, result: Result<(Value, String), String>) -> Task<Message> {
        let Some(write) = self.settings.modules.package_write.take() else {
            return Task::none();
        };
        let (method, params) = (write.method(), write.params());
        let module = match &write {
            PackageWrite::Uninstall(module) | PackageWrite::FolderRemove(module) => {
                Some(module.clone())
            }
            _ => None,
        };
        match result {
            Ok((answer, request)) => {
                // The `module.installed` event this request announced is read and skipped.
                self.read_back(request);
                self.status.text = write.said(&answer);
                let tab = &mut self.settings.modules;
                let row = answer
                    .get("status")
                    .cloned()
                    .and_then(|status| serde_json::from_value::<PackageRow>(status).ok());
                if let (Some(row), Some(read)) = (row, tab.read.as_mut()) {
                    read.packages.adopt(row);
                }
                tab.review = None;
                tab.uninstall = None;
                tab.refusal = None;
                // The module's row is selected once the read lists it.
                if let Some(id) = answer["module"].as_str() {
                    tab.focus = Some(ModulesFocus::Module(id.to_owned()));
                }
                tab.answered = Some(json!({"method": method, "params": params, "answer": answer}));
                let answered = tab.answered.clone();
                self.event("module_package_written", || answered.unwrap_or_default());
            }
            Err(reason) => {
                // The write may have landed before its answer was lost: the poll reads its event
                // like another client's.
                self.resync();
                self.status.text = format!("Could not change the installed modules: {reason}");
                let tab = &mut self.settings.modules;
                tab.answered = Some(json!({"method": method, "params": params, "error": reason}));
                if let Some(review) = tab.review.as_mut().filter(|review| review.installing) {
                    review.installing = false;
                    review.failed = Some(reason);
                } else if let Some(ask) = tab.uninstall.as_mut().filter(|ask| ask.writing) {
                    ask.writing = false;
                    ask.failed = Some(reason);
                } else {
                    tab.refusal = Some((module, reason));
                }
                let answered = tab.answered.clone();
                self.event("module_package_failed", || answered.unwrap_or_default());
            }
        }
        if self.settings.closing && self.settings.idle() {
            return self.close();
        }
        self.read_modules_tab()
    }

    /// Relaunch: once nothing is in flight, quit through the ordinary close path, which stores the
    /// window's frame and the last preference and closes the catalog, then start this desktop
    /// again with the same arguments, so it opens the same catalog and lands on the photograph
    /// Develop had open. An evidence run records the command it would start and quits nothing, so
    /// its harness launches the next run itself.
    fn relaunch_for_modules(&mut self) -> Task<Message> {
        if !self.settings.modules.idle() {
            self.status.text = "Relaunch waits for the change in flight".to_owned();
            return Task::none();
        }
        let Some(launch) = relaunch::SameLaunch::current() else {
            self.status.text = "Luxforge could not find its own program to relaunch".to_owned();
            return Task::none();
        };
        let evidence = self.evidence.is_some();
        let record = json!({"command": launch.record(), "evidence": evidence});
        self.settings.modules.relaunch = Some(record.clone());
        self.event("module_relaunch", || record);
        if evidence {
            self.status.text =
                "Relaunch requested: an evidence run records it and keeps running".to_owned();
            return Task::none();
        }
        if !relaunch::relaunch_same(launch) {
            self.status.text = "A relaunch is already waiting".to_owned();
            return Task::none();
        }
        self.status.text = "Relaunching\u{2026}".to_owned();
        relaunch::request_quit();
        Task::none()
    }

    /// What a captured frame records of the external modules in the open Modules tab: the store,
    /// the pending changes, each external row as read, the review sheet and the confirmation as
    /// drawn, the write in flight, the last answer and what Relaunch did.
    pub(crate) fn external_modules_summary(&self) -> Value {
        let tab = &self.settings.modules;
        let model = self.workspace.settings.modules.as_ref();
        let packages = tab.read.as_ref().map(|read| &read.packages);
        let rows: Vec<Value> = packages
            .into_iter()
            .flat_map(|packages| &packages.modules)
            .map(|row| {
                json!({"module": row.module, "origin": row.origin, "state": format!("{:?}", row.state),
                       "reason": row.reason, "program": {"state": row.program.state, "adapter": row.program.adapter, "reason": row.program.reason},
                       "installed": row.installed.as_ref().and_then(|side| side.version.clone()),
                       "running": row.running.as_ref().and_then(|side| side.version.clone())})
            })
            .collect();
        let review = tab.review.as_ref().map(|review| {
            let drawn = crate::state::external_modules::derive_review(review);
            let (state, refusal, module) = match &review.state {
                ReviewState::Inspecting => ("inspecting", None, None),
                ReviewState::Inspected(inspected) => {
                    ("inspected", None, Some(inspected.manifest.id.clone()))
                }
                ReviewState::Refused { refusal, .. } => ("refused", refusal.clone(), None),
            };
            json!({"path": review.path, "state": state, "refusal": refusal, "module": module,
                   "title": drawn.title, "lead": drawn.lead, "facts": drawn.facts,
                   "warning": drawn.warning, "note": drawn.note,
                   "confirm": drawn.confirm.map(|confirm| json!({"label": confirm.label,
                       "destructive": confirm.destructive, "enabled": confirm.enabled})),
                   "dismiss": drawn.dismiss, "busy": drawn.busy})
        });
        let uninstall = model
            .and_then(|model| model.uninstall.as_ref())
            .map(|drawn| {
                json!({"module": tab.uninstall.as_ref().map(|ask| ask.module.clone()),
                   "title": drawn.title, "body": drawn.body, "failed": drawn.failed,
                   "busy": drawn.busy})
            });
        json!({
            "store": packages.map(|packages| json!(packages.store.state)),
            "store_problem": model.and_then(|model| model.store_problem.clone()),
            "pending": model.and_then(|model| model.pending.clone()),
            "install": model.is_some_and(|model| model.install),
            "load_folder": model.is_some_and(|model| model.load_folder),
            "rows": rows,
            "review": review,
            "uninstall": uninstall,
            "writing": tab.package_write.as_ref().map(PackageWrite::method),
            "answered": tab.answered,
            "relaunch": tab.relaunch,
            "picking": tab.picking,
        })
    }
}
