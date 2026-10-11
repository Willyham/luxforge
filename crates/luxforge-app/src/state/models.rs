//! The Settings sheet's Models tab: every model a registered module holds through a
//! `local-runtime` capability, with its state, size, trade-off, any use restriction and what it
//! needs of the machine; the models loaded now; and one line naming this machine's memory against
//! each model's minimum ([design](../../../../docs/design/ai-editing.md),
//! [contract](../../../../docs/design/module-capabilities.md#local-runtime)). Everything here is
//! read from the descriptors and the `module.status` answers the capability store holds; every
//! button sends a request any client can send, `module.resource.remove` or
//! `module.runtime.unload`, and Copy as JSON request copies exactly that request. Features have no
//! default model tiers yet, so the tab shows none.
use super::{
    MenuTarget,
    capabilities::{CapabilityStore, ModuleCapabilities, human_bytes},
};
use luxforge_core::{
    ModuleDescriptor,
    capabilities::{
        descriptor::{CapabilityKind, ResourceDescriptor},
        resources::{self, ResourceState},
        runtime,
    },
};
use serde_json::{Value, json};

/// The request one Models tab button sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelRequest {
    Remove,
    Unload,
}

impl ModelRequest {
    /// The method and parameters the button sends, as Copy as JSON request copies them. Remove
    /// carries `mutation`, the envelope a press would send.
    pub(crate) fn request(self, module_id: &str, resource_id: &str, mutation: Value) -> Value {
        match self {
            Self::Remove => json!({
                "method": resources::REMOVE,
                "params": {"module_id": module_id, "resource_id": resource_id,
                           "mutation": mutation},
            }),
            Self::Unload => json!({
                "method": runtime::UNLOAD,
                "params": {"module_id": module_id, "resource_id": resource_id},
            }),
        }
    }
}

/// One known model.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelRow {
    pub(crate) module_id: String,
    pub(crate) resource_id: String,
    pub(crate) title: String,
    /// `v1 · 8.8 KB · 2 files`.
    pub(crate) detail: String,
    /// `Not installed`, `Installing 35%`, `Installed`, `Loaded` or `Failed: …`.
    pub(crate) state: String,
    pub(crate) tradeoff: Option<String>,
    pub(crate) use_restriction: Option<String>,
    /// `Needs 2 GB of memory · 64 MB while loaded · cpu · fp32`.
    pub(crate) requirement: Option<String>,
    /// `false` when this machine has less memory than the model's minimum.
    pub(crate) fits: Option<bool>,
    /// Remove is offered: the model is installed, or a partial download is kept.
    pub(crate) removable: bool,
    /// Nothing is in flight for the module, so the row's button may send.
    pub(crate) enabled: bool,
    pub(crate) menu_open: bool,
}

/// One model in the resident set.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResidentRow {
    pub(crate) module_id: String,
    pub(crate) resource_id: String,
    pub(crate) title: String,
    /// `Loaded · 64 MB · cpu on the CPU · fp32` or `Loading · 64 MB`.
    pub(crate) detail: String,
    pub(crate) enabled: bool,
    pub(crate) menu_open: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ModelsModel {
    pub(crate) rows: Vec<ModelRow>,
    pub(crate) resident: Vec<ResidentRow>,
    /// `64 MB of 16 GB held by loaded models`, once a status has been read.
    pub(crate) resident_line: Option<String>,
    /// `This machine has 16 GB of memory. Proof segmenter needs 2 GB.`
    pub(crate) machine: Option<String>,
    /// No module's status has been read yet.
    pub(crate) loading: bool,
    /// Why a module's status could not be read.
    pub(crate) errors: Vec<String>,
}

/// The modules that hold a model, in registry order.
pub(crate) fn model_modules(
    modules: &[ModuleDescriptor],
) -> impl Iterator<Item = &ModuleDescriptor> {
    modules.iter().filter(|module| !held(module).is_empty())
}

/// The resources `module` holds through `local-runtime` capabilities, in declaration order.
fn held(module: &ModuleDescriptor) -> Vec<&ResourceDescriptor> {
    module
        .resources
        .iter()
        .filter(|resource| {
            module.capabilities.iter().any(|capability| {
                matches!(&capability.kind, CapabilityKind::LocalRuntime { resource: held, .. } if *held == resource.id)
            })
        })
        .collect()
}

/// Memory as the tab names it: whole gigabytes when it is a whole number of them.
pub(crate) fn memory_text(bytes: u64) -> String {
    const GIB: u64 = 1 << 30;
    if bytes >= GIB && bytes % GIB == 0 {
        format!("{} GB", bytes / GIB)
    } else {
        human_bytes(bytes)
    }
}

/// The tab, from the descriptors and the statuses the store holds.
pub(crate) fn derive(
    modules: &[ModuleDescriptor],
    store: &CapabilityStore,
    menu: Option<&MenuTarget>,
) -> ModelsModel {
    let mut model = ModelsModel::default();
    let mut machine_memory = None;
    let mut minimums = Vec::new();
    let mut read_any = false;
    let mut budget = None;
    for module in model_modules(modules) {
        let state = store.module(&module.id);
        let status = state.status.as_ref();
        read_any |= status.is_some();
        if status.is_none()
            && let Some(message) = &state.message
        {
            model.errors.push(format!("{}: {message}", module.title));
        }
        if let Some(memory) = status.and_then(|status| status.machine.as_ref()?.memory_bytes) {
            machine_memory = Some(memory);
        }
        if let Some(runtime) = status.and_then(|status| status.runtime.as_ref()) {
            budget = Some((runtime.held_bytes, runtime.budget_bytes));
        }
        for resource in held(module) {
            if let Some(minimum) = resource.min_memory_bytes {
                minimums.push((resource.title.clone(), minimum));
            }
            model.rows.push(row(module, resource, state, menu));
        }
        for resident in status
            .and_then(|status| status.runtime.as_ref())
            .map(|runtime| runtime.models.as_slice())
            .unwrap_or_default()
        {
            let title = module
                .resources
                .iter()
                .find(|declared| declared.id == resident.resource_id)
                .map_or_else(
                    || resident.resource_id.clone(),
                    |declared| declared.title.clone(),
                );
            let mut detail = vec![
                if resident.state == "loaded" {
                    "Loaded"
                } else {
                    "Loading"
                }
                .to_owned(),
                human_bytes(resident.resident_bytes),
            ];
            if let Some(placement) = &resident.placement {
                detail.push(placement.backend.clone());
                detail.push(format!("{:?}", placement.precision).to_lowercase());
            }
            if resident.in_use {
                detail.push("in use".into());
            }
            model.resident.push(ResidentRow {
                module_id: module.id.clone(),
                resource_id: resident.resource_id.clone(),
                title,
                detail: detail.join(" · "),
                enabled: state.pending == 0,
                menu_open: menu
                    == Some(&MenuTarget::Resident {
                        module_id: module.id.clone(),
                        resource_id: resident.resource_id.clone(),
                    }),
            });
        }
    }
    model.loading = !read_any && model.errors.is_empty();
    model.resident_line = budget.map(|(held, budget)| {
        format!(
            "{} of {} held by loaded models",
            human_bytes(held),
            memory_text(budget)
        )
    });
    model.machine = read_any.then(|| machine_line(machine_memory, &minimums));
    model
}

/// One line: this machine's memory, then each model's minimum, and which of them it cannot hold.
fn machine_line(memory: Option<u64>, minimums: &[(String, u64)]) -> String {
    let mut line = match memory {
        Some(memory) => format!("This machine has {} of memory.", memory_text(memory)),
        None => "This machine's memory is not known, so no minimum is checked.".to_owned(),
    };
    let needs: Vec<String> = minimums
        .iter()
        .map(|(title, minimum)| {
            let short = memory.is_some_and(|memory| memory < *minimum);
            format!(
                "{title} needs {}{}",
                memory_text(*minimum),
                if short { ", more than it has" } else { "" }
            )
        })
        .collect();
    if !needs.is_empty() {
        line.push(' ');
        line.push_str(&needs.join("; "));
        line.push('.');
    }
    line
}

fn row(
    module: &ModuleDescriptor,
    resource: &ResourceDescriptor,
    state: &ModuleCapabilities,
    menu: Option<&MenuTarget>,
) -> ModelRow {
    let status = state.status.as_ref();
    let reported =
        status.and_then(|status| status.resources.iter().find(|row| row.id == resource.id));
    let loaded = status
        .and_then(|status| status.runtime.as_ref())
        .and_then(|runtime| {
            runtime
                .models
                .iter()
                .find(|model| model.resource_id == resource.id)
        })
        .map(|model| model.state.as_str());
    let files = resource.files.len() + 1;
    let mut detail = format!(
        "v{} · {}",
        resource.version,
        human_bytes(resource.total_bytes())
    );
    if files > 1 {
        detail.push_str(&format!(" · {files} files"));
    }
    let resource_state = reported.map(|row| row.state);
    let staged = reported
        .and_then(|row| row.staged_bytes)
        .filter(|bytes| *bytes > 0);
    let text = match (resource_state, loaded) {
        (None, _) => "Not read yet".to_owned(),
        (Some(ResourceState::Installed), Some("loaded")) => "Loaded".to_owned(),
        (Some(ResourceState::Installed), Some(_)) => "Loading".to_owned(),
        (Some(ResourceState::Installed), None) => "Installed".to_owned(),
        (Some(ResourceState::Installing), _) => {
            let fraction = reported
                .and_then(|row| row.job_id.as_ref())
                .and_then(|job| {
                    state
                        .jobs
                        .iter()
                        .chain(
                            status
                                .map(|status| status.jobs.as_slice())
                                .unwrap_or_default(),
                        )
                        .find(|record| &record.job_id == job)
                })
                .and_then(|record| record.progress.fraction);
            match fraction {
                Some(fraction) => format!("Installing {:.0}%", fraction.clamp(0.0, 1.0) * 100.0),
                None => "Installing".to_owned(),
            }
        }
        (Some(ResourceState::NotInstalled | ResourceState::Failed), _) => {
            let mut text = match reported.and_then(|row| row.error.as_ref()) {
                Some(error) if resource_state == Some(ResourceState::Failed) => {
                    format!("Failed: {}", error.message)
                }
                _ => "Not installed".to_owned(),
            };
            if let Some(staged) = staged {
                text.push_str(&format!(
                    " · {} of {} downloaded, resumes on the next download",
                    human_bytes(staged),
                    human_bytes(resource.total_bytes())
                ));
            }
            text
        }
    };
    let requirement = reported
        .and_then(|row| row.requirements.as_ref())
        .map(|needs| {
            let mut parts = Vec::new();
            if let Some(minimum) = needs.min_memory_bytes {
                parts.push(format!("Needs {} of memory", memory_text(minimum)));
            }
            if let Some(resident) = needs.resident_bytes {
                parts.push(format!("{} while loaded", human_bytes(resident)));
            }
            if !needs.backends.is_empty() {
                parts.push(needs.backends.join(" or "));
            }
            if let Some(precision) = needs.precision {
                parts.push(format!("{precision:?}").to_lowercase());
            }
            parts.join(" · ")
        })
        .filter(|text| !text.is_empty());
    ModelRow {
        module_id: module.id.clone(),
        resource_id: resource.id.clone(),
        title: resource.title.clone(),
        detail,
        state: text,
        tradeoff: resource.tradeoff.clone(),
        use_restriction: resource.use_restriction.clone(),
        requirement,
        fits: reported.and_then(|row| row.fits),
        removable: resource_state == Some(ResourceState::Installed) || staged.is_some(),
        enabled: state.pending == 0 && module.is_available(),
        menu_open: menu
            == Some(&MenuTarget::Model {
                module_id: module.id.clone(),
                resource_id: resource.id.clone(),
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::capabilities::{ModuleStatus, ResidentModel, RuntimeStatus};

    fn module() -> ModuleDescriptor {
        serde_json::from_value(json!({
            "id": "m", "title": "M", "effects": [], "actions": [], "controls": [], "canvas": null,
            "availability": {"kind": "available"},
            "capabilities": [
                {"id": "d", "kind": "download-artifact", "resource": "seg", "purpose": "p"},
                {"id": "r", "kind": "local-runtime", "resource": "seg", "model": "session",
                 "resident_bytes": 67108864, "backends": ["coreml", "cpu"], "precision": "fp16",
                 "purpose": "p"},
                {"id": "o", "kind": "download-artifact", "resource": "other", "purpose": "p"},
            ],
            "resources": [
                {"id": "seg", "title": "Segmenter", "version": "2", "url": "https://e.example/a",
                 "bytes": 1024, "sha256": "a".repeat(64), "format": "f", "license": "l",
                 "provenance": "p",
                 "files": [{"name": "decoder", "url": "https://e.example/b", "bytes": 1024,
                            "sha256": "b".repeat(64)}],
                 "tradeoff": "Finds smaller objects; slower to start",
                 "use_restriction": "Non-commercial use only",
                 "min_memory_bytes": 8589934592u64},
                {"id": "other", "title": "Other", "version": "1", "url": "https://e.example/c",
                 "bytes": 3, "sha256": "c".repeat(64), "format": "f", "license": "l",
                 "provenance": "p"},
            ],
        }))
        .unwrap()
    }

    fn status(resource: Value, models: Vec<ResidentModel>, memory: Option<u64>) -> ModuleStatus {
        ModuleStatus {
            resources: vec![serde_json::from_value(resource).unwrap()],
            permissions: Default::default(),
            jobs: Vec::new(),
            runtime: Some(RuntimeStatus {
                held_bytes: models.iter().map(|model| model.resident_bytes).sum(),
                budget_bytes: 16 << 30,
                available: true,
                models,
            }),
            machine: Some(crate::state::capabilities::MachineStatus {
                memory_bytes: memory,
            }),
        }
    }

    fn row_json(state: &str, extra: Value) -> Value {
        let mut row = json!({
            "id": "seg", "title": "Segmenter", "version": "2", "bytes": 2048,
            "sha256": "a".repeat(64), "license": "l", "provenance": "p",
            "url": "https://e.example/a", "state": state,
            "requirements": {"model": "session", "backends": ["coreml", "cpu"],
                             "precision": "fp16", "resident_bytes": 67108864,
                             "min_memory_bytes": 8589934592u64},
            "fits": true,
        });
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    }

    #[test]
    fn only_a_model_is_listed_and_an_unread_tab_is_loading() {
        let modules = [module()];
        let model = derive(&modules, &CapabilityStore::default(), None);
        assert_eq!(
            model.rows.len(),
            1,
            "the resource no local-runtime holds is no model"
        );
        assert!(model.loading);
        assert_eq!(model.machine, None);
        let row = &model.rows[0];
        assert_eq!(row.detail, "v2 · 2.0 KB · 2 files");
        assert_eq!(row.state, "Not read yet");
        assert_eq!(
            row.tradeoff.as_deref(),
            Some("Finds smaller objects; slower to start")
        );
        assert_eq!(
            row.use_restriction.as_deref(),
            Some("Non-commercial use only")
        );
        assert!(!row.removable);
    }

    #[test]
    fn an_installed_loaded_model_is_listed_resident_with_its_requirement_and_the_machine() {
        let modules = [module()];
        let mut store = CapabilityStore::default();
        store.module_mut("m").status = Some(status(
            row_json("installed", json!({})),
            vec![ResidentModel {
                resource_id: "seg".into(),
                state: "loaded".into(),
                resident_bytes: 64 << 20,
                placement: Some(luxforge_core::inference::Placement {
                    backend: "cpu".into(),
                    device: luxforge_core::inference::Device::Cpu,
                    precision: luxforge_core::inference::Precision::Fp16,
                    partitions: None,
                }),
                in_use: false,
            }],
            Some(36 << 30),
        ));
        let menu = MenuTarget::Resident {
            module_id: "m".into(),
            resource_id: "seg".into(),
        };
        let model = derive(&modules, &store, Some(&menu));
        let row = &model.rows[0];
        assert_eq!(row.state, "Loaded");
        assert!(row.removable && row.enabled && !row.menu_open);
        assert_eq!(
            row.requirement.as_deref(),
            Some("Needs 8 GB of memory · 64.0 MB while loaded · coreml or cpu · fp16")
        );
        assert_eq!(model.resident.len(), 1);
        assert_eq!(model.resident[0].detail, "Loaded · 64.0 MB · cpu · fp16");
        assert!(model.resident[0].menu_open);
        assert_eq!(
            model.resident_line.as_deref(),
            Some("64.0 MB of 16 GB held by loaded models")
        );
        assert_eq!(
            model.machine.as_deref(),
            Some("This machine has 36 GB of memory. Segmenter needs 8 GB.")
        );
        assert!(!model.loading);
    }

    #[test]
    fn a_short_machine_and_a_kept_partial_download_are_said() {
        let modules = [module()];
        let mut store = CapabilityStore::default();
        store.module_mut("m").status = Some(status(
            row_json("not-installed", json!({"fits": false, "staged_bytes": 512})),
            Vec::new(),
            Some(4 << 30),
        ));
        let model = derive(&modules, &store, None);
        let row = &model.rows[0];
        assert_eq!(row.fits, Some(false));
        assert_eq!(
            row.state,
            "Not installed · 512 B of 2.0 KB downloaded, resumes on the next download"
        );
        assert!(row.removable, "a kept partial download can be removed");
        assert_eq!(
            model.machine.as_deref(),
            Some("This machine has 4 GB of memory. Segmenter needs 8 GB, more than it has.")
        );
        assert!(model.resident.is_empty());
    }

    #[test]
    fn each_button_copies_the_request_it_sends() {
        let mutation = json!({"request_id": "r1", "actor": "desktop"});
        assert_eq!(
            ModelRequest::Unload.request("m", "seg", mutation.clone()),
            json!({"method": "module.runtime.unload", "params": {"module_id": "m", "resource_id": "seg"}})
        );
        assert_eq!(
            ModelRequest::Remove.request("m", "seg", mutation.clone()),
            json!({"method": "module.resource.remove",
                   "params": {"module_id": "m", "resource_id": "seg", "mutation": mutation}})
        );
    }
}
