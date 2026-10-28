//! The Settings sheet's Models tab: every model a registered module holds through a
//! `local-runtime` capability, with its state, size, trade-off, any use restriction and what it
//! needs of the machine; the models loaded now; and one line naming this machine's memory against
//! each model's minimum ([design](../../../../docs/design/ai-editing.md),
//! [contract](../../../../docs/design/module-capabilities.md#local-runtime)). Everything here is
//! read from the descriptors and the `module.status` answers the capability store holds. A model an
//! offered setup holds is listed as that setup's part, captioned with the tools that use it
//! ([design](../../../../docs/design/ai-tools-workspace.md#settings--models)), and any other model
//! as itself. Every button sends requests any client can send — `module.resource.remove` for each
//! resource, `module.runtime.unload`, or Download…'s `module.setup.install` — and Copy as JSON
//! request copies exactly those. Features have no default model tiers yet, so the tab shows none.
use super::{
    MenuTarget,
    capabilities::{CapabilityStore, ModuleCapabilities, human_bytes, version_label},
    fill_setup::{self, SetupAction},
    settings::resource_offered,
};
use luxforge_core::{
    ModuleDescriptor,
    capabilities::{
        descriptor::{CapabilityKind, ResourceDescriptor, SetupDescriptor, SetupPart},
        resources::{self, ResourceRow, ResourceState},
        runtime,
    },
    flags::FlagList,
    jobs::JobStatus,
};
use serde_json::{Value, json};

/// The request one Models tab button sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelRequest {
    Remove,
    Unload,
}

impl ModelRequest {
    /// The requests the button sends for each of `resources`, in order, as Copy as JSON request
    /// copies them; `mutation` makes each Remove's envelope.
    pub(crate) fn requests(
        self,
        module_id: &str,
        resources: &[String],
        mut mutation: impl FnMut() -> Value,
    ) -> Vec<Value> {
        resources
            .iter()
            .map(|resource| self.request(module_id, resource, mutation()))
            .collect()
    }

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

/// One known model: a setup's part (the object finder's encoder and decoder as one row), or a
/// model no offered setup holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ModelRow {
    pub(crate) module_id: String,
    /// The row's identity: its first resource.
    pub(crate) resource_id: String,
    /// Every resource the row stands for.
    pub(crate) resources: Vec<String>,
    pub(crate) title: String,
    /// The tools that use a part: `Corrections › Object, Masks › Object`.
    pub(crate) caption: Option<String>,
    /// `v1 · 8.8 KB · 2 files`.
    pub(crate) detail: String,
    /// `Not installed`, `Installing 35%`, `Installing · preparing the model`, `Installed`,
    /// `Loaded` or `Failed: …`.
    pub(crate) state: String,
    pub(crate) tradeoff: Option<String>,
    pub(crate) use_restriction: Option<String>,
    /// `Needs 2 GB of memory · 64 MB while loaded · cpu · fp32`.
    pub(crate) requirement: Option<String>,
    /// `false` when this machine has less memory than the model's minimum.
    pub(crate) fits: Option<bool>,
    /// Remove is offered: the model is installed, or a partial download is kept.
    pub(crate) removable: bool,
    /// What Remove removes: each resource that is installed or partly downloaded.
    pub(crate) remove: Vec<String>,
    /// Download… on a part that is not installed: the install of the smallest offered setup that
    /// holds it, which opens that setup's sheet.
    pub(crate) download: Option<SetupAction>,
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

/// The modules that hold a model the desktop offers, in registry order.
pub(crate) fn model_modules<'a>(
    modules: &'a [ModuleDescriptor],
    flags: Option<&'a FlagList>,
) -> impl Iterator<Item = &'a ModuleDescriptor> + 'a {
    modules
        .iter()
        .filter(move |module| !held(module, flags).is_empty())
}

/// The resources `module` holds through `local-runtime` capabilities, in declaration order: those
/// behind an experiment flag only while it is on, so a model nobody can use yet is not listed.
pub(crate) fn held<'a>(
    module: &'a ModuleDescriptor,
    flags: Option<&FlagList>,
) -> Vec<&'a ResourceDescriptor> {
    module
        .resources
        .iter()
        .filter(|resource| {
            module.capabilities.iter().any(|capability| {
                matches!(&capability.kind, CapabilityKind::LocalRuntime { resource: held, .. } if *held == resource.id)
            })
        })
        .filter(|resource| resource_offered(flags, resource))
        .collect()
}

/// Memory as the tab names it: whole gigabytes when it is a whole number of them.
pub(crate) fn memory_text(bytes: u64) -> String {
    const GIB: u64 = 1 << 30;
    if bytes >= GIB && bytes.is_multiple_of(GIB) {
        format!("{} GB", bytes / GIB)
    } else {
        human_bytes(bytes)
    }
}

/// The tab, from the descriptors and the statuses the store holds.
pub(crate) fn derive(
    modules: &[ModuleDescriptor],
    flags: Option<&FlagList>,
    store: &CapabilityStore,
    menu: Option<&MenuTarget>,
) -> ModelsModel {
    let mut model = ModelsModel::default();
    let mut machine_memory = None;
    let mut minimums = Vec::new();
    let mut read_any = false;
    let mut budget = None;
    for module in model_modules(modules, flags) {
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
        // A resource an offered setup holds is listed as that setup's part, once; any other as
        // itself.
        let mut parts_listed: Vec<&str> = Vec::new();
        for resource in held(module, flags) {
            match part_of(module, &resource.id, flags) {
                Some(part) if parts_listed.contains(&part.name.as_str()) => {}
                Some(part) => {
                    parts_listed.push(&part.name);
                    if let Some(minimum) = part_minimum(module, part) {
                        minimums.push((part.name.clone(), minimum));
                    }
                    model.rows.push(part_row(module, part, state, flags, menu));
                }
                None => {
                    if let Some(minimum) = resource.min_memory_bytes {
                        minimums.push((resource.title.clone(), minimum));
                    }
                    model.rows.push(row(module, resource, state, menu));
                }
            }
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

/// The part of the first offered setup, in declared order, that holds `resource`.
fn part_of<'a>(
    module: &'a ModuleDescriptor,
    resource: &str,
    flags: Option<&FlagList>,
) -> Option<&'a SetupPart> {
    module
        .setups
        .iter()
        .filter(|setup| fill_setup::offered(module, setup, flags))
        .flat_map(|setup| &setup.parts)
        .find(|part| part.resources.iter().any(|held| held == resource))
}

/// The largest minimum of a part's resources.
fn part_minimum(module: &ModuleDescriptor, part: &SetupPart) -> Option<u64> {
    part.resources
        .iter()
        .filter_map(|id| declared(module, id)?.min_memory_bytes)
        .max()
}

fn declared<'a>(module: &'a ModuleDescriptor, id: &str) -> Option<&'a ResourceDescriptor> {
    module.resources.iter().find(|resource| resource.id == id)
}

/// Where a setup's tool lives, as a part's caption names it.
fn tool_label(setup: &SetupDescriptor) -> String {
    match setup.id.as_str() {
        fill_setup::REMOVE => "Corrections \u{203a} Remove".into(),
        fill_setup::OBJECT => "Corrections \u{203a} Object".into(),
        fill_setup::MASK_OBJECT => "Masks \u{203a} Object".into(),
        fill_setup::REPLACE => "Corrections \u{203a} Replace".into(),
        _ => setup.title.clone(),
    }
}

/// `Needs 2 GB of memory · 64 MB while loaded · cpu · fp32`, from a model's requirements.
fn requirement_text(
    minimum: Option<u64>,
    resident: Option<u64>,
    backends: &[String],
    precision: Option<String>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(minimum) = minimum {
        parts.push(format!("Needs {} of memory", memory_text(minimum)));
    }
    if let Some(resident) = resident {
        parts.push(format!("{} while loaded", human_bytes(resident)));
    }
    if !backends.is_empty() {
        parts.push(backends.join(" or "));
    }
    parts.extend(precision);
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// One setup part's row: its resources as one model, captioned with the offered tools that use
/// it, with one size, one state, one Remove that removes each resource, and Download… while it is
/// not installed.
fn part_row(
    module: &ModuleDescriptor,
    part: &SetupPart,
    state: &ModuleCapabilities,
    flags: Option<&FlagList>,
    menu: Option<&MenuTarget>,
) -> ModelRow {
    let status = state.status.as_ref();
    let rows: Vec<&ResourceRow> = part
        .resources
        .iter()
        .filter_map(|id| status?.resources.iter().find(|row| &row.id == id))
        .collect();
    let read = status.is_some() && rows.len() == part.resources.len();
    let bytes: u64 = part
        .resources
        .iter()
        .filter_map(|id| declared(module, id))
        .map(ResourceDescriptor::total_bytes)
        .sum();
    let files: usize = part
        .resources
        .iter()
        .filter_map(|id| declared(module, id))
        .map(|resource| resource.files.len() + 1)
        .sum();
    let mut detail = human_bytes(bytes);
    if files > 1 {
        detail.push_str(&format!(" \u{b7} {files} files"));
    }
    let staged: u64 = rows
        .iter()
        .filter(|row| row.state != ResourceState::Installed)
        .filter_map(|row| row.staged_bytes)
        .sum();
    let missing: u64 = rows
        .iter()
        .filter(|row| row.state != ResourceState::Installed)
        .map(|row| row.bytes)
        .sum();
    let all = |wanted: ResourceState| rows.iter().all(|row| row.state == wanted);
    let any = |wanted: ResourceState| rows.iter().any(|row| row.state == wanted);
    let loaded: Vec<&str> = status
        .and_then(|status| status.runtime.as_ref())
        .map(|runtime| {
            runtime
                .models
                .iter()
                .filter(|model| part.resources.contains(&model.resource_id))
                .map(|model| model.state.as_str())
                .collect()
        })
        .unwrap_or_default();
    let installing_job = rows
        .iter()
        .filter(|row| row.state == ResourceState::Installing)
        .filter_map(|row| row.job_id.as_ref())
        .find_map(|id| {
            state
                .jobs
                .iter()
                .chain(
                    status
                        .map(|status| status.jobs.as_slice())
                        .unwrap_or_default(),
                )
                .find(|record| &record.job_id == id && record.status == JobStatus::Running)
        });
    let text = if !read {
        "Not read yet".to_owned()
    } else if all(ResourceState::Installed) {
        match loaded.as_slice() {
            [] => "Installed".to_owned(),
            states if states.contains(&"loaded") => "Loaded".to_owned(),
            _ => "Loading".to_owned(),
        }
    } else if any(ResourceState::Installing) {
        let progress = installing_job.map(|record| &record.progress);
        match (
            progress.and_then(|progress| progress.fraction),
            progress.and_then(|progress| progress.message.as_deref()),
        ) {
            (_, Some(step)) if step != "downloading" => format!("Installing \u{b7} {step}"),
            _ => {
                // One percentage over the part: what is installed, staged or downloading of it.
                let done: f64 = rows
                    .iter()
                    .map(|row| match row.state {
                        ResourceState::Installed => row.bytes as f64,
                        _ => row.staged_bytes.unwrap_or(0) as f64,
                    })
                    .sum::<f64>()
                    .max(
                        installing_job
                            .and_then(|record| record.progress.fraction)
                            .zip(installing_job.and_then(|record| record.resource_id.as_ref()))
                            .and_then(|(fraction, id)| {
                                let row = rows.iter().find(|row| &row.id == id)?;
                                Some(fraction.clamp(0.0, 1.0) * row.bytes as f64)
                            })
                            .unwrap_or(0.0),
                    );
                let total = rows.iter().map(|row| row.bytes).sum::<u64>().max(1);
                format!("Installing {:.0}%", done / total as f64 * 100.0)
            }
        }
    } else {
        let failure = rows
            .iter()
            .filter(|row| row.state == ResourceState::Failed)
            .find_map(|row| row.error.as_ref());
        let mut text = match failure {
            Some(error) => format!("Failed: {}", error.message),
            None => "Not installed".to_owned(),
        };
        if staged > 0 {
            text.push_str(&format!(
                " \u{b7} {} of {} downloaded, resumes on the next download",
                human_bytes(staged),
                human_bytes(missing)
            ));
        }
        text
    };
    let requirements: Vec<_> = rows
        .iter()
        .filter_map(|row| row.requirements.as_ref())
        .collect();
    let requirement = requirement_text(
        requirements
            .iter()
            .filter_map(|needs| needs.min_memory_bytes)
            .max(),
        requirements
            .iter()
            .filter_map(|needs| needs.resident_bytes)
            .reduce(|one, two| one.saturating_add(two)),
        requirements
            .iter()
            .map(|needs| needs.backends.as_slice())
            .find(|backends| !backends.is_empty())
            .unwrap_or_default(),
        requirements
            .iter()
            .find_map(|needs| needs.precision)
            .map(|precision| format!("{precision:?}").to_lowercase()),
    );
    let fits = if rows.iter().any(|row| row.fits == Some(false)) {
        Some(false)
    } else {
        rows.iter().find_map(|row| row.fits)
    };
    let remove: Vec<String> = rows
        .iter()
        .filter(|row| {
            row.state == ResourceState::Installed || row.staged_bytes.is_some_and(|bytes| bytes > 0)
        })
        .map(|row| row.id.clone())
        .collect();
    let first = declared(module, &part.resources[0]);
    // Download… while nothing of it installs and this machine can run it.
    let download = (read
        && !all(ResourceState::Installed)
        && !any(ResourceState::Installing)
        && fits != Some(false))
    .then(|| fill_setup::smallest_holding(module, &part.resources[0], flags))
    .flatten()
    .map(|setup| SetupAction::Install {
        module_id: module.id.clone(),
        setup: setup.id.clone(),
    });
    let tools: Vec<String> = module
        .setups
        .iter()
        .filter(|setup| fill_setup::offered(module, setup, flags))
        .filter(|setup| setup.parts.iter().any(|held| held.name == part.name))
        .map(tool_label)
        .collect();
    let id = part.resources[0].clone();
    ModelRow {
        module_id: module.id.clone(),
        menu_open: menu
            == Some(&MenuTarget::Model {
                module_id: module.id.clone(),
                resource_id: id.clone(),
            }),
        resource_id: id,
        resources: part.resources.clone(),
        title: part.name.clone(),
        caption: (!tools.is_empty()).then(|| tools.join(", ")),
        detail,
        state: text,
        tradeoff: first.and_then(|resource| resource.tradeoff.clone()),
        use_restriction: part
            .resources
            .iter()
            .find_map(|id| declared(module, id)?.use_restriction.clone()),
        requirement,
        fits,
        removable: !remove.is_empty(),
        remove,
        download,
        enabled: state.pending == 0 && module.is_available(),
    }
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
        "{} · {}",
        version_label(&resource.version),
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
            let progress = reported
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
                .map(|record| &record.progress);
            // Past the download, the step the install is on: checking the format, then preparing
            // the model for this machine (the Core ML copy on a Mac).
            match (
                progress.and_then(|progress| progress.fraction),
                progress.and_then(|progress| progress.message.as_deref()),
            ) {
                (_, Some(step)) if step != "downloading" => format!("Installing · {step}"),
                (Some(fraction), _) => {
                    format!("Installing {:.0}%", fraction.clamp(0.0, 1.0) * 100.0)
                }
                (None, _) => "Installing".to_owned(),
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
        .and_then(|needs| {
            requirement_text(
                needs.min_memory_bytes,
                needs.resident_bytes,
                &needs.backends,
                needs
                    .precision
                    .map(|precision| format!("{precision:?}").to_lowercase()),
            )
        });
    let removable = resource_state == Some(ResourceState::Installed) || staged.is_some();
    ModelRow {
        module_id: module.id.clone(),
        resource_id: resource.id.clone(),
        resources: vec![resource.id.clone()],
        title: resource.title.clone(),
        caption: None,
        detail,
        state: text,
        tradeoff: resource.tradeoff.clone(),
        use_restriction: resource.use_restriction.clone(),
        requirement,
        fits: reported.and_then(|row| row.fits),
        removable,
        remove: if removable {
            vec![resource.id.clone()]
        } else {
            Vec::new()
        },
        download: None,
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
    use crate::state::capabilities::{ModuleStatus, ResidentModel, RuntimeStatus, resource_detail};

    fn module() -> ModuleDescriptor {
        serde_json::from_value(json!({
            "id": "m", "title": "M", "effects": [], "actions": [], "controls": [], "canvas": null,
            "toolset": "adjustments", "availability": {"kind": "available"},
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
            setups: Vec::new(),
            permissions: Default::default(),
            jobs: Vec::new(),
            fill_tiers: Vec::new(),
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
    fn a_version_that_names_its_files_reads_without_a_v() {
        assert_eq!(version_label("2"), "v2");
        assert_eq!(version_label("1.4"), "v1.4");
        assert_eq!(
            version_label("klein-4b-q4_0-3b1f5a9d-lora-0e3f5879-cond-ba26d15a"),
            "klein-4b-q4_0-3b1f5a9d-lora-0e3f5879-cond-ba26d15a"
        );
        assert_eq!(version_label(""), "");
    }

    #[test]
    fn a_tools_panel_resource_row_names_only_a_numbered_version() {
        assert_eq!(resource_detail("1", 20), "v1 · 20 B");
        assert_eq!(
            resource_detail("klein-4b-q4_0-3b1f5a9d-lora-0e3f5879-cond-ba26d15a", 20),
            "20 B"
        );
    }

    #[test]
    fn only_a_model_is_listed_and_an_unread_tab_is_loading() {
        let modules = [module()];
        let model = derive(&modules, None, &CapabilityStore::default(), None);
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
        let model = derive(&modules, None, &store, Some(&menu));
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
        let model = derive(&modules, None, &store, None);
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
    fn an_install_says_its_percentage_then_the_step_past_the_download() {
        let modules = [module()];
        let mut store = CapabilityStore::default();
        let at = |fraction: f64, step: &str| {
            let mut status = status(
                row_json(
                    "installing",
                    json!({"job_id": "job-0123456789abcdef0123456789abcdef"}),
                ),
                Vec::new(),
                Some(36 << 30),
            );
            status.jobs = vec![
                serde_json::from_value(json!({
                    "job_id": "job-0123456789abcdef0123456789abcdef", "kind": "install", "status": "running",
                    "ownership": "catalog", "module_id": "m", "resource_id": "seg",
                    "progress": {"fraction": fraction, "message": step},
                }))
                .unwrap(),
            ];
            status
        };
        store.module_mut("m").status = Some(at(0.35, "downloading"));
        assert_eq!(
            derive(&modules, None, &store, None).rows[0].state,
            "Installing 35%"
        );
        store.module_mut("m").status = Some(at(1.0, "preparing the model"));
        assert_eq!(
            derive(&modules, None, &store, None).rows[0].state,
            "Installing · preparing the model"
        );
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
