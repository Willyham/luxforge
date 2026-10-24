//! Setting up an AI tool: one group line per setup a module declares, as the tool that needs it
//! shows it where its Set up would be ([design](../../../../docs/design/ai-tools-workspace.md#setting-up)).
//!
//! Everything is derived from what the desktop already holds of the module: its `module.status`
//! answer's `setups` (each setup's state, missing and staged bytes, memory minimum and fit, its
//! parts and the install jobs it names), the install jobs the desktop tracks, and the jobs the
//! desktop's own last install of each setup queued ([`ModuleCapabilities::installs`]), so one bar
//! covers what that install set out to fetch. The copy names the tool, never a model: the model's
//! name, version and provenance are in Settings › Models.
//!
//! The machinery is keyed by setup id alone, so any tool shows its line with one call to
//! [`group`] and opens its sheet with one [`SetupAction::Install`]. Every action is one or more
//! requests any client sends ([`SetupAction::requests`]): `module.setup.install`, through the
//! setup sheet when a grant is missing, or `job.cancel` for each of the setup's install jobs.
use crate::state::{
    capabilities::{ModuleCapabilities, capitalised, human_bytes, job},
    models::{held, memory_text},
    remove::fill_module,
    settings::resource_offered,
};
use luxforge_core::{
    ModuleDescriptor,
    capabilities::{
        descriptor::SetupDescriptor,
        host::INSTALL_SETUP,
        resources::ResourceState,
        setups::{SetupPartRow, SetupResourceRow, SetupRow, SetupState},
    },
    flags::FlagList,
    jobs::{JOB_CANCEL, JobStatus},
};
use serde_json::{Value, json};

/// The setup Corrections › Remove needs: the generative fill.
pub(crate) const REMOVE: &str = "remove";
/// The setup Corrections › Object needs: the object finder and the generative fill.
pub(crate) const OBJECT: &str = "object";
/// The setup Masks › Object needs: the object finder.
pub(crate) const MASK_OBJECT: &str = "mask-object";
/// Replace's add-on, offered once Remove's fill is installed.
pub(crate) const REPLACE: &str = "replace";

/// What a setup button asks the capability driver for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SetupAction {
    /// Install what the setup still lacks: `module.setup.install`, which opens the setup sheet
    /// when a grant is missing.
    Install { module_id: String, setup: String },
    /// Stop every install job of the setup: one `job.cancel` each.
    Cancel {
        module_id: String,
        setup: String,
        jobs: Vec<String>,
    },
}

impl SetupAction {
    /// Cancel stops work already running, so it stays pressable while other requests are out.
    pub(crate) fn always_pressable(&self) -> bool {
        matches!(self, Self::Cancel { .. })
    }

    pub(crate) fn module_id(&self) -> &str {
        match self {
            Self::Install { module_id, .. } | Self::Cancel { module_id, .. } => module_id,
        }
    }

    /// The requests the button sends, in order, exactly as Copy as JSON request copies them:
    /// `mutation` makes each envelope, one per press.
    pub(crate) fn requests(&self, mut mutation: impl FnMut() -> Value) -> Vec<Value> {
        match self {
            Self::Install { module_id, setup } => vec![json!({
                "method": INSTALL_SETUP,
                "params": {"module_id": module_id, "setup": setup, "mutation": mutation()},
            })],
            Self::Cancel { jobs, .. } => jobs
                .iter()
                .map(|job| json!({"method": JOB_CANCEL, "params": {"job_id": job}}))
                .collect(),
        }
    }
}

/// One part's state under the group's line, while the setup installs or after it failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PartLine {
    pub(crate) name: String,
    /// `Ready ✓`, `Downloading · 34%`, `Waiting`, `Paused at 1.1 GB of 2.5 GB`, `Didn't finish`.
    pub(crate) text: String,
    pub(crate) ready: bool,
}

/// One setup's line in its tool's group: what the tool needs or can do now, and the one thing to
/// press.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetupLine {
    pub(crate) setup: String,
    /// The setup's state; `None` until the status is read.
    pub(crate) state: Option<SetupState>,
    pub(crate) text: String,
    /// `One-time download · 2.5 GB`, `Download paused at 1.1 GB of 2.5 GB`, `35% of 2.7 GB` or the
    /// install's step.
    pub(crate) detail: Option<String>,
    /// The one bar's fraction while it installs.
    pub(crate) progress: Option<f64>,
    /// Each part's state, for a setup of more than one part while it installs or after it failed.
    pub(crate) parts: Vec<PartLine>,
    /// The button's label and what it sends.
    pub(crate) action: Option<(String, SetupAction)>,
    /// Every resource of the setup is installed.
    pub(crate) installed: bool,
    /// A few plain words for a status bar or a disabled Generate: `Set up AI editing first`.
    pub(crate) reason: String,
}

impl SetupLine {
    /// What a captured frame records of the line, with the requests its button sends.
    pub(crate) fn summary(&self) -> Value {
        json!({
            "setup": self.setup,
            "state": self.state,
            "text": self.text,
            "detail": self.detail,
            "progress": self.progress,
            "parts": self.parts.iter().map(|part| json!({
                "name": part.name, "text": part.text, "ready": part.ready,
            })).collect::<Vec<_>>(),
            "action": self.action.as_ref().map(|(label, _)| label.clone()),
            "requests": self.action.as_ref().map(|(_, action)| {
                action.requests(|| json!({"request_id": "<per press>", "actor": crate::state::ACTOR}))
            }),
            "installed": self.installed,
            "reason": self.reason,
        })
    }
}

/// The fill module's setups as Remove shows them: Remove's own, then Replace's once Remove's is in.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SetupModel {
    pub(crate) module_id: String,
    /// The `remove` setup: the model that removes and fills.
    pub(crate) main: SetupLine,
    /// The `replace` setup, offered only once the main one is installed.
    pub(crate) addon: Option<SetupLine>,
    /// Nothing is in flight for the module, so its buttons may send; Cancel always may.
    pub(crate) enabled: bool,
    /// The resources the setups stand for, which the generic resource rows leave out: every model
    /// the module holds, as Settings › Models lists them.
    pub(crate) covers: Vec<String>,
}

impl SetupModel {
    pub(crate) fn summary(&self) -> Value {
        json!({
            "main": self.main.summary(),
            "addon": self.addon.as_ref().map(SetupLine::summary),
            "enabled": self.enabled,
        })
    }
}

/// A tool's words for its setup.
struct Words {
    /// What the tool is called where a sentence names it: `AI editing`, `Object removal`.
    feature: String,
    /// What it does, while it is missing.
    pitch: String,
    /// The button that starts it.
    set_up: &'static str,
    /// The download's detail, from its size.
    download: fn(&str) -> String,
    installing: String,
    /// The start of a failure's line.
    failed: &'static str,
    /// Why the tool waits, while it is not set up.
    reason: String,
    ready: String,
    ready_reason: String,
}

fn one_time(size: &str) -> String {
    format!("One-time download \u{b7} {size}")
}

fn plain(size: &str) -> String {
    format!("{size} download")
}

/// The words of `setup`'s tool. Remove and Replace keep the delivered words; Object's are the
/// design's; any other setup speaks in its declared title.
fn words(module: &ModuleDescriptor, setup: &SetupDescriptor) -> Words {
    let title = &module.title;
    match setup.id.as_str() {
        REMOVE => Words {
            feature: title.clone(),
            pitch: format!(
                "Remove objects and fill in parts of a photo. {title} runs on this computer, so \
                 your photos stay on it."
            ),
            set_up: "Set up",
            download: one_time,
            installing: format!("Setting up {title}\u{2026}"),
            failed: "Setup didn't finish",
            reason: format!("Set up {title} first"),
            ready: format!(
                "{title} is ready. Choose Remove in Corrections and paint over what to remove."
            ),
            ready_reason: format!("{title} is ready"),
        },
        REPLACE => Words {
            feature: "Replace".into(),
            pitch: "Add Replace to fill with what you describe.".into(),
            set_up: "Add",
            download: plain,
            installing: "Adding Replace\u{2026}".into(),
            failed: "Replace didn't install",
            reason: "Add Replace to fill with what you describe".into(),
            ready: "Replace is ready. Type what to fill with under Remove.".into(),
            ready_reason: "Replace is ready".into(),
        },
        OBJECT => Words {
            feature: "Object removal".into(),
            pitch: "Point at an object and Luxforge removes it, filling the space to match the \
                    photo. It runs on this computer, so your photos stay on it."
                .into(),
            set_up: "Set up\u{2026}",
            download: one_time,
            installing: "Setting up Object\u{2026}".into(),
            failed: "Setup didn't finish",
            reason: "Set up Object first".into(),
            ready: "Object is ready. Point at an object to remove it.".into(),
            ready_reason: "Object is ready".into(),
        },
        MASK_OBJECT => Words {
            feature: "Object selection".into(),
            pitch: "Point at an object to select it for a mask. It runs on this computer, so your \
                    photos stay on it."
                .into(),
            set_up: "Set up\u{2026}",
            download: one_time,
            installing: "Setting up Object\u{2026}".into(),
            failed: "Setup didn't finish",
            reason: "Set up Object first".into(),
            ready: "Object is ready. Point at an object to select it.".into(),
            ready_reason: "Object is ready".into(),
        },
        _ => Words {
            feature: setup.title.clone(),
            pitch: format!("{} runs on this computer.", setup.title),
            set_up: "Set up\u{2026}",
            download: one_time,
            installing: format!("Setting up {}\u{2026}", setup.title),
            failed: "Setup didn't finish",
            reason: format!("Set up {} first", setup.title),
            ready: format!("{} is ready.", setup.title),
            ready_reason: format!("{} is ready", setup.title),
        },
    }
}

/// What this machine is called in a sentence.
pub(crate) fn this_machine() -> &'static str {
    if cfg!(target_os = "macos") {
        "Mac"
    } else {
        "computer"
    }
}

/// Whether the desktop offers `setup`: every resource it holds is offered, so a setup holding the
/// object finder is offered only while `ai.analysis` is on.
pub(crate) fn offered(
    module: &ModuleDescriptor,
    setup: &SetupDescriptor,
    flags: Option<&FlagList>,
) -> bool {
    setup.resources().all(|id| {
        module
            .resources
            .iter()
            .find(|declared| declared.id == id)
            .is_some_and(|declared| resource_offered(flags, declared))
    })
}

/// The smallest offered setup holding `resource` (the fewest resources, the first declared of
/// equals): the one Settings › Models' Download… opens.
pub(crate) fn smallest_holding<'a>(
    module: &'a ModuleDescriptor,
    resource: &str,
    flags: Option<&FlagList>,
) -> Option<&'a SetupDescriptor> {
    module
        .setups
        .iter()
        .filter(|setup| offered(module, setup, flags))
        .filter(|setup| setup.resources().any(|held| held == resource))
        .min_by_key(|setup| setup.resources().count())
}

/// A line with only its text and reason.
fn said(setup: &str, state: Option<SetupState>, text: String, reason: String) -> SetupLine {
    SetupLine {
        setup: setup.to_owned(),
        state,
        text,
        detail: None,
        progress: None,
        parts: Vec::new(),
        action: None,
        installed: false,
        reason,
    }
}

/// The running install job of a resource, as the desktop or the status last read it.
fn running<'a>(
    state: &'a ModuleCapabilities,
    resource: &SetupResourceRow,
) -> Option<&'a luxforge_core::jobs::JobRecord> {
    let id = resource.job_id.as_ref()?;
    job(state, id.as_str()).filter(|record| record.status == JobStatus::Running)
}

/// What one bar covers of these resources and how much of it is done: every resource still
/// missing, and those the desktop's last install of `setup` queued and has since installed.
fn bar<'a>(
    state: &ModuleCapabilities,
    setup: &str,
    resources: impl Iterator<Item = &'a SetupResourceRow>,
) -> (u64, f64) {
    let started = |id: &str| {
        state
            .installs
            .get(setup)
            .is_some_and(|jobs| jobs.iter().any(|(resource, _)| resource == id))
    };
    let (mut total, mut done) = (0u64, 0f64);
    for resource in resources {
        let staged = resource.staged_bytes.unwrap_or(0) as f64;
        match resource.state {
            ResourceState::Installed if started(&resource.id) => {
                total += resource.bytes;
                done += resource.bytes as f64;
            }
            ResourceState::Installed => {}
            ResourceState::Installing => {
                total += resource.bytes;
                let fraction = running(state, resource)
                    .and_then(|record| record.progress.fraction)
                    .map_or(0.0, |fraction| fraction.clamp(0.0, 1.0));
                done += staged.max(fraction * resource.bytes as f64);
            }
            ResourceState::NotInstalled | ResourceState::Failed => {
                total += resource.bytes;
                done += staged;
            }
        }
    }
    (total, done.min(total as f64))
}

/// The step a running install of these resources is on past its download, capitalised.
fn step<'a>(
    state: &ModuleCapabilities,
    mut resources: impl Iterator<Item = &'a SetupResourceRow>,
) -> Option<String> {
    resources.find_map(|resource| {
        let message = running(state, resource)?.progress.message.as_deref()?;
        (message != "downloading").then(|| capitalised(message))
    })
}

fn percent(total: u64, done: f64) -> f64 {
    if total == 0 { 0.0 } else { done / total as f64 }
}

/// One part's state under the line.
fn part_line(state: &ModuleCapabilities, setup: &str, part: &SetupPartRow) -> PartLine {
    let ready = part.state == SetupState::Installed;
    let text = match part.state {
        SetupState::Installed => "Ready \u{2713}".to_owned(),
        SetupState::Installing => {
            let downloading = part
                .resources
                .iter()
                .any(|resource| running(state, resource).is_some());
            match step(state, part.resources.iter()) {
                Some(step) => step,
                None if downloading => {
                    let (total, done) = bar(state, setup, part.resources.iter());
                    format!("Downloading \u{b7} {:.0}%", percent(total, done) * 100.0)
                }
                None => "Waiting".to_owned(),
            }
        }
        SetupState::Failed => "Didn't finish".to_owned(),
        SetupState::Missing if part.staged_bytes > 0 => format!(
            "Paused at {} of {}",
            human_bytes(part.staged_bytes),
            human_bytes(part.missing_bytes)
        ),
        SetupState::Missing => "Not downloaded".to_owned(),
        SetupState::CannotRun => format!(
            "Needs {} of memory",
            memory_text(part.minimum_memory.unwrap_or_default())
        ),
        SetupState::Unavailable => "Can't run here".to_owned(),
    };
    PartLine {
        name: part.name.clone(),
        text,
        ready,
    }
}

/// Each part's state, for a setup of more than one part.
fn parts(state: &ModuleCapabilities, row: &SetupRow) -> Vec<PartLine> {
    if row.parts.len() < 2 {
        return Vec::new();
    }
    row.parts
        .iter()
        .map(|part| part_line(state, &row.id, part))
        .collect()
}

/// `setup`'s line in its tool's group, or `None` when `module` declares no such setup. Before the
/// status is read it is checking; afterwards it follows the setup's state.
pub(crate) fn group(
    module: &ModuleDescriptor,
    state: &ModuleCapabilities,
    setup: &str,
) -> Option<SetupLine> {
    let declared = module.setup(setup)?;
    let words = words(module, declared);
    let row = state
        .status
        .as_ref()
        .and_then(|status| status.setups.iter().find(|row| row.id == setup));
    let Some(row) = row else {
        let checking = format!("Checking {}\u{2026}", module.title);
        return Some(said(setup, None, checking.clone(), checking));
    };
    let module_id = &module.id;
    let install = |label: &str| {
        Some((
            label.to_owned(),
            SetupAction::Install {
                module_id: module_id.clone(),
                setup: setup.to_owned(),
            },
        ))
    };
    let paused = (row.staged_bytes > 0).then(|| {
        format!(
            "Download paused at {} of {}",
            human_bytes(row.staged_bytes),
            human_bytes(row.missing_bytes)
        )
    });
    let size = human_bytes(row.missing_bytes);
    let state_of = Some(row.state);
    Some(match row.state {
        SetupState::Installed => SetupLine {
            installed: true,
            ..said(setup, state_of, words.ready, words.ready_reason)
        },
        SetupState::CannotRun => {
            let needs = format!(
                "{} needs a computer with {} of memory",
                words.feature,
                memory_text(row.minimum_memory.unwrap_or_default())
            );
            let has = state
                .status
                .as_ref()
                .and_then(|status| status.machine.as_ref()?.memory_bytes);
            let text = match has {
                Some(has) => format!("{needs}. This {} has {}.", this_machine(), memory_text(has)),
                None => format!("{needs}."),
            };
            said(setup, state_of, text, needs)
        }
        SetupState::Unavailable => {
            let cannot = format!("{} can't run on this computer", words.feature);
            said(setup, state_of, format!("{cannot}."), cannot)
        }
        SetupState::Installing => {
            let (total, done) = bar(state, setup, row.parts.iter().flat_map(|p| &p.resources));
            let fraction = percent(total, done);
            let jobs: Vec<String> = row
                .jobs
                .iter()
                .map(|job| job.job_id.as_str().to_owned())
                .collect();
            SetupLine {
                detail: step(state, row.parts.iter().flat_map(|part| &part.resources)).or_else(
                    || {
                        (total > 0)
                            .then(|| format!("{:.0}% of {}", fraction * 100.0, human_bytes(total)))
                    },
                ),
                progress: Some(fraction),
                parts: parts(state, row),
                action: (!jobs.is_empty()).then(|| {
                    (
                        "Cancel".to_owned(),
                        SetupAction::Cancel {
                            module_id: module_id.clone(),
                            setup: setup.to_owned(),
                            jobs,
                        },
                    )
                }),
                ..said(setup, state_of, words.installing.clone(), words.installing)
            }
        }
        SetupState::Failed => SetupLine {
            detail: Some(paused.unwrap_or_else(|| (words.download)(&size))),
            parts: parts(state, row),
            action: install("Try again"),
            ..said(
                setup,
                state_of,
                match &row.error {
                    Some(error) => format!("{}: {}", words.failed, error.message),
                    None => format!("{}.", words.failed),
                },
                words.reason,
            )
        },
        SetupState::Missing => SetupLine {
            action: install(if paused.is_some() {
                "Resume"
            } else {
                words.set_up
            }),
            detail: Some(paused.unwrap_or_else(|| (words.download)(&size))),
            ..said(setup, state_of, words.pitch, words.reason)
        },
    })
}

/// The setups of `module`, when it is the fill module the desktop offers; `None` for every other.
pub(crate) fn derive(
    module: &ModuleDescriptor,
    state: &ModuleCapabilities,
    flags: Option<&FlagList>,
) -> Option<SetupModel> {
    fill_module(std::slice::from_ref(module), flags)?;
    let main = group(module, state, REMOVE).unwrap_or_else(|| {
        let checking = format!("Checking {}\u{2026}", module.title);
        said(REMOVE, None, checking.clone(), checking)
    });
    let addon = group(module, state, REPLACE).filter(|_| main.installed);
    Some(SetupModel {
        module_id: module.id.clone(),
        main,
        addon,
        enabled: state.pending == 0 && module.is_available(),
        covers: held(module, flags)
            .into_iter()
            .map(|resource| resource.id.clone())
            .collect(),
    })
}

/// What a captured frame records of every offered setup of `module`, by id.
pub(crate) fn summary(
    module: &ModuleDescriptor,
    state: &ModuleCapabilities,
    flags: Option<&FlagList>,
) -> Value {
    Value::Object(
        module
            .setups
            .iter()
            .filter(|setup| offered(module, setup, flags))
            .filter_map(|setup| {
                let line = group(module, state, &setup.id)?;
                Some((setup.id.clone(), line.summary()))
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::capabilities::{MachineStatus, ModuleStatus};

    const GIB: u64 = 1 << 30;
    const MIB: u64 = 1 << 20;
    const JOB: &str = "job-0123456789abcdef0123456789abcdef";
    const JOB2: &str = "job-fedcba9876543210fedcba9876543210";

    /// A fill module with the generative fill `gen` (2.5 GB), Replace's `text` (2.3 GB) and the
    /// object finder's `enc` and `dec` behind `ai.analysis`, in the four setups.
    fn module() -> ModuleDescriptor {
        let resource = |id: &str, bytes: u64, flag: Option<&str>| {
            json!({"id": id, "title": format!("Technical {id}"), "version": "1",
                   "url": "https://e.example/a", "bytes": bytes, "sha256": "a".repeat(64),
                   "format": "f", "license": "l", "provenance": "p", "flag": flag})
        };
        let introduction = json!({"eyebrow": "AI EDITING", "title": "t", "lead": "l",
                                  "hero": "describe", "promise": "p"});
        let setup = |id: &str, parts: Value| json!({"id": id, "title": id, "introduction": introduction, "parts": parts});
        let fill = json!({"name": "Generative fill", "purpose": "p", "resources": ["gen"]});
        let finder = json!({"name": "Object finder", "purpose": "p", "resources": ["enc", "dec"]});
        serde_json::from_value(json!({
            "id": "ai", "title": "AI editing", "effects": [], "actions": [], "controls": [],
            "canvas": null, "availability": {"kind": "available"},
            "tasks": [{"id": "fill-generative", "title": "Generative fill", "notes": "n"}],
            "resources": [
                resource("gen", 2_720_267_065, None),
                resource("text", 2_508_703_966, None),
                resource("enc", 100 * MIB, Some("ai.analysis")),
                resource("dec", 33 * MIB, Some("ai.analysis")),
            ],
            "setups": [
                setup("remove", json!([fill])),
                setup("object", json!([finder, fill])),
                setup("mask-object", json!([finder])),
                setup("replace", json!([{"name": "Replace", "purpose": "p", "resources": ["text"]}])),
            ],
        }))
        .unwrap()
    }

    /// One resource of a part, in `state`.
    fn resource(id: &str, bytes: u64, state: &str, extra: Value) -> Value {
        let mut row = json!({"id": id, "state": state, "bytes": bytes});
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    }

    /// A setup row with `parts` (name, state, resources) and `extra` fields.
    fn row(id: &str, state: &str, parts: Vec<(&str, &str, Vec<Value>)>, extra: Value) -> Value {
        let parts: Vec<Value> = parts
            .into_iter()
            .map(|(name, state, resources)| {
                let bytes: u64 = resources.iter().map(|r| r["bytes"].as_u64().unwrap()).sum();
                let missing: u64 = resources
                    .iter()
                    .filter(|r| r["state"] != "installed")
                    .map(|r| r["bytes"].as_u64().unwrap())
                    .sum();
                let staged: u64 = resources
                    .iter()
                    .filter_map(|r| r["staged_bytes"].as_u64())
                    .sum();
                json!({"name": name, "state": state, "bytes": bytes, "missing_bytes": missing,
                       "staged_bytes": staged, "resources": resources})
            })
            .collect();
        let missing: u64 = parts
            .iter()
            .map(|p| p["missing_bytes"].as_u64().unwrap())
            .sum();
        let staged: u64 = parts
            .iter()
            .map(|p| p["staged_bytes"].as_u64().unwrap())
            .sum();
        let mut row = json!({"id": id, "state": state, "missing_bytes": missing,
                             "staged_bytes": staged, "parts": parts});
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    }

    fn state(rows: Vec<Value>, jobs: Vec<Value>, memory: Option<u64>) -> ModuleCapabilities {
        let status = ModuleStatus {
            resources: Vec::new(),
            setups: rows
                .into_iter()
                .map(|row| serde_json::from_value(row).unwrap())
                .collect(),
            permissions: Default::default(),
            jobs: jobs
                .into_iter()
                .map(|job| serde_json::from_value(job).unwrap())
                .collect(),
            fill_tiers: Vec::new(),
            runtime: None,
            machine: Some(MachineStatus {
                memory_bytes: memory,
            }),
        };
        ModuleCapabilities {
            status: Some(status),
            ..ModuleCapabilities::default()
        }
    }

    fn running(id: &str, resource: &str, fraction: f64, step: &str) -> Value {
        json!({"job_id": id, "kind": "install", "status": "running", "ownership": "catalog",
               "module_id": "ai", "resource_id": resource,
               "progress": {"fraction": fraction, "message": step}})
    }

    fn line(state: &ModuleCapabilities, setup: &str) -> SetupLine {
        group(&module(), state, setup).expect("a declared setup")
    }

    fn label(line: &SetupLine) -> Option<&str> {
        line.action.as_ref().map(|(label, _)| label.as_str())
    }

    fn install(setup: &str) -> SetupAction {
        SetupAction::Install {
            module_id: "ai".into(),
            setup: setup.into(),
        }
    }

    const GEN: u64 = 2_720_267_065;

    fn missing_remove(extra: Value) -> Value {
        row(
            "remove",
            "missing",
            vec![(
                "Generative fill",
                "missing",
                vec![resource("gen", GEN, "not-installed", extra)],
            )],
            json!({}),
        )
    }

    #[test]
    fn a_setup_the_module_does_not_declare_has_no_line_and_an_unread_one_is_checking() {
        assert_eq!(
            group(&module(), &ModuleCapabilities::default(), "other"),
            None
        );
        let checking = line(&ModuleCapabilities::default(), OBJECT);
        assert_eq!(checking.text, "Checking AI editing\u{2026}");
        assert_eq!(checking.state, None);
        assert_eq!(checking.action, None);
    }

    #[test]
    fn only_a_fill_module_has_a_setup_model_and_it_covers_every_offered_model() {
        let mut other = module();
        other.tasks.clear();
        assert_eq!(derive(&other, &ModuleCapabilities::default(), None), None);
        let setup = derive(&module(), &ModuleCapabilities::default(), None).unwrap();
        assert_eq!(setup.main.text, "Checking AI editing\u{2026}");
        assert_eq!(setup.addon, None);
    }

    #[test]
    fn a_setup_holding_a_flagged_resource_is_offered_only_while_its_flag_is_on() {
        let module = module();
        let offered_ids = |flags: Option<&FlagList>| {
            module
                .setups
                .iter()
                .filter(|setup| offered(&module, setup, flags))
                .map(|setup| setup.id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(offered_ids(None), ["remove", "replace"]);
        assert_eq!(smallest_holding(&module, "gen", None).unwrap().id, "remove");
        assert_eq!(smallest_holding(&module, "enc", None), None);
    }

    #[test]
    fn missing_it_says_what_the_tool_does_and_offers_only_the_missing_bytes() {
        let fresh = line(
            &state(vec![missing_remove(json!({}))], vec![], None),
            REMOVE,
        );
        assert_eq!(
            fresh.text,
            "Remove objects and fill in parts of a photo. AI editing runs on this computer, so \
             your photos stay on it."
        );
        assert_eq!(
            fresh.detail.as_deref(),
            Some("One-time download \u{b7} 2.5 GB")
        );
        assert_eq!(fresh.action, Some(("Set up".to_owned(), install(REMOVE))));
        assert_eq!(fresh.reason, "Set up AI editing first");
        assert!(fresh.parts.is_empty());
        // The fill installed, Object fetches only the finder.
        let object = line(
            &state(
                vec![row(
                    "object",
                    "missing",
                    vec![
                        (
                            "Object finder",
                            "missing",
                            vec![
                                resource("enc", 100 * MIB, "not-installed", json!({})),
                                resource("dec", 33 * MIB, "not-installed", json!({})),
                            ],
                        ),
                        (
                            "Generative fill",
                            "installed",
                            vec![resource("gen", GEN, "installed", json!({}))],
                        ),
                    ],
                    json!({}),
                )],
                vec![],
                None,
            ),
            OBJECT,
        );
        assert_eq!(
            object.detail.as_deref(),
            Some("One-time download \u{b7} 133.0 MB")
        );
        assert_eq!(label(&object), Some("Set up\u{2026}"));
    }

    #[test]
    fn partly_downloaded_it_offers_resume() {
        let paused = line(
            &state(
                vec![missing_remove(json!({"staged_bytes": 1_288_490_189u64}))],
                vec![],
                None,
            ),
            REMOVE,
        );
        assert_eq!(
            paused.detail.as_deref(),
            Some("Download paused at 1.2 GB of 2.5 GB")
        );
        assert_eq!(paused.action, Some(("Resume".to_owned(), install(REMOVE))));
    }

    #[test]
    fn installing_it_shows_one_bar_each_part_and_cancels_every_job() {
        let at = |installs: bool| {
            let mut state = state(
                vec![row(
                    "object",
                    "installing",
                    vec![
                        (
                            "Object finder",
                            "installing",
                            vec![
                                resource("enc", 100 * MIB, "installed", json!({})),
                                resource("dec", 100 * MIB, "installing", json!({"job_id": JOB})),
                            ],
                        ),
                        (
                            "Generative fill",
                            "installing",
                            vec![resource(
                                "gen",
                                200 * MIB,
                                "installing",
                                json!({"job_id": JOB2}),
                            )],
                        ),
                    ],
                    json!({"jobs": [
                        {"resource_id": "dec", "job_id": JOB, "status": "running"},
                        {"resource_id": "gen", "job_id": JOB2, "status": "queued"},
                    ]}),
                )],
                vec![
                    running(JOB, "dec", 0.5, "downloading"),
                    json!({"job_id": JOB2, "kind": "install", "status": "queued",
                           "ownership": "catalog", "module_id": "ai", "resource_id": "gen",
                           "progress": {}}),
                ],
                None,
            );
            if installs {
                state.installs.insert(
                    OBJECT.into(),
                    vec![
                        ("enc".into(), "job-a".into()),
                        ("dec".into(), JOB.into()),
                        ("gen".into(), JOB2.into()),
                    ],
                );
            }
            line(&state, OBJECT)
        };
        // The desktop's own install queued all three: the bar covers 400 MB, 150 MB done.
        let ours = at(true);
        assert_eq!(ours.text, "Setting up Object\u{2026}");
        assert_eq!(ours.detail.as_deref(), Some("38% of 400.0 MB"));
        assert_eq!(ours.progress, Some(0.375));
        assert_eq!(
            ours.parts,
            vec![
                PartLine {
                    name: "Object finder".into(),
                    text: "Downloading \u{b7} 75%".into(),
                    ready: false
                },
                PartLine {
                    name: "Generative fill".into(),
                    text: "Waiting".into(),
                    ready: false
                },
            ]
        );
        assert_eq!(
            ours.action,
            Some((
                "Cancel".to_owned(),
                SetupAction::Cancel {
                    module_id: "ai".into(),
                    setup: OBJECT.into(),
                    jobs: vec![JOB.into(), JOB2.into()],
                }
            ))
        );
        // Another client's install: the bar covers only what is still missing.
        assert_eq!(at(false).detail.as_deref(), Some("17% of 300.0 MB"));
    }

    #[test]
    fn past_the_download_it_names_the_step() {
        let installing = line(
            &state(
                vec![row(
                    "remove",
                    "installing",
                    vec![(
                        "Generative fill",
                        "installing",
                        vec![resource("gen", GEN, "installing", json!({"job_id": JOB}))],
                    )],
                    json!({"jobs": [{"resource_id": "gen", "job_id": JOB, "status": "running"}]}),
                )],
                vec![running(JOB, "gen", 1.0, "preparing the model")],
                None,
            ),
            REMOVE,
        );
        assert_eq!(installing.detail.as_deref(), Some("Preparing the model"));
        assert_eq!(installing.progress, Some(1.0));
        assert!(installing.parts.is_empty(), "one part lists no parts");
    }

    #[test]
    fn a_failed_setup_names_the_first_failure_each_part_and_tries_again() {
        let failed = line(
            &state(
                vec![row(
                    "object",
                    "failed",
                    vec![
                        (
                            "Object finder",
                            "installed",
                            vec![
                                resource("enc", 100 * MIB, "installed", json!({})),
                                resource("dec", 33 * MIB, "installed", json!({})),
                            ],
                        ),
                        (
                            "Generative fill",
                            "failed",
                            vec![resource(
                                "gen",
                                GEN,
                                "failed",
                                json!({"staged_bytes": GEN / 2}),
                            )],
                        ),
                    ],
                    json!({"error": {"code": "unavailable", "message": "the connection dropped"}}),
                )],
                vec![],
                None,
            ),
            OBJECT,
        );
        assert_eq!(failed.text, "Setup didn't finish: the connection dropped");
        assert_eq!(
            failed.detail.as_deref(),
            Some("Download paused at 1.3 GB of 2.5 GB")
        );
        assert_eq!(
            failed
                .parts
                .iter()
                .map(|part| part.text.as_str())
                .collect::<Vec<_>>(),
            ["Ready \u{2713}", "Didn't finish"]
        );
        assert_eq!(
            failed.action,
            Some(("Try again".to_owned(), install(OBJECT)))
        );
        let replace = line(
            &state(
                vec![row(
                    "replace",
                    "failed",
                    vec![(
                        "Replace",
                        "failed",
                        vec![resource("text", 2_508_703_966, "failed", json!({}))],
                    )],
                    json!({"error": {"code": "unavailable", "message": "the disk is full"}}),
                )],
                vec![],
                None,
            ),
            REPLACE,
        );
        assert_eq!(replace.text, "Replace didn't install: the disk is full");
        assert_eq!(replace.detail.as_deref(), Some("2.3 GB download"));
    }

    #[test]
    fn a_machine_too_small_or_without_a_runtime_has_nothing_to_press() {
        let small = line(
            &state(
                vec![row(
                    "remove",
                    "cannot-run",
                    vec![(
                        "Generative fill",
                        "cannot-run",
                        vec![resource("gen", GEN, "not-installed", json!({}))],
                    )],
                    json!({"minimum_memory": 16 * GIB, "fits": false, "reason": "memory"}),
                )],
                vec![],
                Some(8 * GIB),
            ),
            REMOVE,
        );
        assert_eq!(
            small.text,
            format!(
                "AI editing needs a computer with 16 GB of memory. This {} has 8 GB.",
                this_machine()
            )
        );
        assert_eq!(
            small.reason,
            "AI editing needs a computer with 16 GB of memory"
        );
        assert_eq!(small.action, None);
        let none = line(
            &state(
                vec![row(
                    "remove",
                    "unavailable",
                    vec![(
                        "Generative fill",
                        "unavailable",
                        vec![resource("gen", GEN, "not-installed", json!({}))],
                    )],
                    json!({"reason": "no-runtime"}),
                )],
                vec![],
                None,
            ),
            REMOVE,
        );
        assert_eq!(none.text, "AI editing can't run on this computer.");
        assert_eq!(none.action, None);
    }

    #[test]
    fn installed_it_is_ready_and_remove_then_offers_replace() {
        let installed = |id: &str, name: &str, res: &str| {
            row(
                id,
                "installed",
                vec![(
                    name,
                    "installed",
                    vec![resource(res, 1, "installed", json!({}))],
                )],
                json!({}),
            )
        };
        let missing_replace = row(
            "replace",
            "missing",
            vec![(
                "Replace",
                "missing",
                vec![resource("text", 2_508_703_966, "not-installed", json!({}))],
            )],
            json!({}),
        );
        let setup = derive(
            &module(),
            &state(
                vec![
                    installed("remove", "Generative fill", "gen"),
                    missing_replace.clone(),
                ],
                vec![],
                None,
            ),
            None,
        )
        .unwrap();
        assert!(setup.main.installed);
        assert_eq!(
            setup.main.text,
            "AI editing is ready. Choose Remove in Corrections and paint over what to remove."
        );
        assert_eq!(setup.main.action, None);
        let addon = setup.addon.expect("Replace, once Remove is in");
        assert_eq!(addon.text, "Add Replace to fill with what you describe.");
        assert_eq!(addon.detail.as_deref(), Some("2.3 GB download"));
        assert_eq!(addon.action, Some(("Add".to_owned(), install(REPLACE))));
        let waiting = derive(
            &module(),
            &state(
                vec![missing_remove(json!({})), missing_replace],
                vec![],
                None,
            ),
            None,
        )
        .unwrap();
        assert_eq!(waiting.addon, None, "Replace waits for Remove");
    }

    #[test]
    fn each_action_sends_exactly_the_requests_it_copies() {
        let mut serial = 0;
        let mut mutation = || {
            serial += 1;
            json!({"request_id": format!("r{serial}")})
        };
        assert_eq!(
            install(OBJECT).requests(&mut mutation),
            [json!({"method": "module.setup.install",
                    "params": {"module_id": "ai", "setup": "object", "mutation": {"request_id": "r1"}}})]
        );
        let cancel = SetupAction::Cancel {
            module_id: "ai".into(),
            setup: OBJECT.into(),
            jobs: vec![JOB.into(), JOB2.into()],
        };
        assert_eq!(
            cancel.requests(mutation),
            [
                json!({"method": "job.cancel", "params": {"job_id": JOB}}),
                json!({"method": "job.cancel", "params": {"job_id": JOB2}}),
            ]
        );
        assert!(cancel.always_pressable() && !install(REMOVE).always_pressable());
    }
}
