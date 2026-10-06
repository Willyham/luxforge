//! Module capabilities as the desktop knows them, and the models its tools panel, the consent
//! notice and evidence are derived from. Everything here is plain data: the owner's own answers
//! (`module.settings.read`, `module.status`, `job.read`, a `consent-required` failure) and the
//! text a person is typing into a setting. Nothing here calls the owner, and nothing decides what
//! the host accepts: every value is validated by the core when it is sent.
//!
//! The block is generated from the descriptor, so any module that declares settings, resources or
//! tasks gets the same surface: one row per resource, a settings form of the module-level fields
//! drawn with the tool panel's own control kinds, the permission counts with Revoke all, and one
//! status line. What it does not draw — provider profiles, secrets, single grants — is reached
//! through the `module.*` methods every client calls.
use crate::state::{
    Inputs,
    canvas::{Notice, NoticeAction, NoticeIcon, NoticeTone},
    number::{NumberSpec, number_text},
};
use luxforge_core::{
    AssetId, EditorState, ModuleDescriptor, ParameterKind,
    capabilities::{
        consent::Disclosure,
        descriptor::{AdapterCost, ResourceDescriptor},
        grants::{GrantKind, PermissionCounts},
        resources::{ResourceRow, ResourceState},
        settings::{FieldRead, ProfileStatus, SettingsRead, SettingsState},
    },
    jobs::{JobRecord, JobStatus},
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

/// The most jobs one module's state keeps; finished ones go first.
const MAX_TRACKED: usize = 16;

/// A module the capability surface applies to: it declares settings, resources or tasks.
pub(crate) fn declares(module: &ModuleDescriptor) -> bool {
    module.settings.is_some() || !module.resources.is_empty() || !module.tasks.is_empty()
}

/// `module.status` as the desktop reads it: every declared resource, how many grants and denials
/// are recorded, and the module's recent jobs. The settings are read in full through
/// `module.settings.read` instead.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(crate) struct ModuleStatus {
    #[serde(default)]
    pub(crate) resources: Vec<ResourceRow>,
    #[serde(default)]
    pub(crate) permissions: PermissionCounts,
    #[serde(default)]
    pub(crate) jobs: Vec<JobRecord>,
}

/// `error.data.consent` of a `consent-required` failure: the scope a grant would cover, exactly as
/// the core named it, and what the person is told before deciding.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(crate) struct Consent {
    pub(crate) module_id: String,
    pub(crate) capability: String,
    pub(crate) kind: GrantKind,
    /// Kept as the core sent it, so Allow grants exactly this scope and nothing wider.
    pub(crate) scope: Value,
    pub(crate) disclosure: Disclosure,
    #[serde(default)]
    pub(crate) denied: bool,
}

/// One operation the desktop asks the owner for, as plain data: the app layer turns it into the
/// owner requests an independent JSON client sends. A settings write carries the settings
/// revision it was made against, so a stale one is refused as a conflict rather than applied.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Operation {
    /// Read the module's settings and status.
    Load,
    /// Read the module's status alone.
    Refresh,
    /// Write one module-level setting.
    Set {
        field: String,
        value: Value,
        revision: u64,
    },
    Install {
        resource: String,
    },
    Remove {
        resource: String,
    },
    Cancel {
        job: String,
    },
    /// Withdraw every live grant of the module: `module.permission.list`, then one
    /// `module.permission.revoke` per live grant.
    RevokeAll,
    RunTask {
        task: String,
        asset: Option<AssetId>,
        profile: Option<String>,
    },
    /// The person's answer to a consent notice: Allow grants exactly the scope and retries the
    /// operation that was refused, once; Don't allow records the denial.
    Consent {
        allow: bool,
        consent: Box<Consent>,
        retry: Option<Box<Operation>>,
    },
}

impl Operation {
    /// A short name for events and evidence; never a value.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Refresh => "refresh",
            Self::Set { .. } => "set",
            Self::Install { .. } => "install",
            Self::Remove { .. } => "remove",
            Self::Cancel { .. } => "cancel",
            Self::RevokeAll => "revoke-all",
            Self::RunTask { .. } => "task",
            Self::Consent { allow: true, .. } => "allow",
            Self::Consent { allow: false, .. } => "deny",
        }
    }
}

/// Where one run of a task has got to. A run belongs to the asset it was started for.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TaskPhase {
    /// The request is in flight.
    Requesting,
    /// The core asked for consent; the notice is open.
    Consent,
    /// A job was queued; its progress is the tracked job's.
    Job(String),
    Succeeded {
        job: String,
        artifacts: Vec<String>,
        result: Value,
    },
    Failed {
        code: String,
        message: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TaskRun {
    /// The asset the run was for, when the task takes one.
    pub(crate) asset: Option<AssetId>,
    pub(crate) profile: Option<String>,
    pub(crate) phase: TaskPhase,
}

/// One capability notice the desktop has open: the core's consent request and the operation Allow
/// retries.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenConsent {
    pub(crate) consent: Consent,
    pub(crate) retry: Option<Operation>,
}

/// What the desktop knows about one module's capabilities.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ModuleCapabilities {
    pub(crate) settings: Option<SettingsRead>,
    pub(crate) status: Option<ModuleStatus>,
    /// Text being typed into a setting, by setting id, until it is committed or abandoned.
    pub(crate) edits: BTreeMap<String, String>,
    /// The block's one status line: why the last read or operation failed, or that the settings
    /// changed elsewhere and were read again.
    pub(crate) message: Option<String>,
    /// The capability jobs the desktop started and the module's live ones, oldest first.
    pub(crate) jobs: Vec<JobRecord>,
    /// The newest run of each task, by task id.
    pub(crate) tasks: BTreeMap<String, TaskRun>,
    /// Owner requests in flight for this module.
    pub(crate) pending: u32,
}

/// The state of a module nothing has been read for yet.
static UNREAD: ModuleCapabilities = ModuleCapabilities {
    settings: None,
    status: None,
    edits: BTreeMap::new(),
    message: None,
    jobs: Vec::new(),
    tasks: BTreeMap::new(),
    pending: 0,
};

impl ModuleCapabilities {
    /// Some tracked job is still queued or running.
    pub(crate) fn live(&self) -> bool {
        self.jobs.iter().any(|job| !job.status.is_finished())
    }

    /// Record what the owner said about a job: replace a tracked one, and start tracking one that
    /// is still live or that this desktop started (`started`), whatever its status. At most
    /// [`MAX_TRACKED`] are kept, finished ones going first, oldest first.
    pub(crate) fn track(&mut self, record: JobRecord, started: bool) {
        match self.jobs.iter_mut().find(|job| job.job_id == record.job_id) {
            Some(tracked) => *tracked = record,
            None if started || !record.status.is_finished() => self.jobs.push(record),
            None => return,
        }
        while self.jobs.len() > MAX_TRACKED {
            match self.jobs.iter().position(|job| job.status.is_finished()) {
                Some(oldest) => {
                    self.jobs.remove(oldest);
                }
                None => break,
            }
        }
    }

    /// The settings revision a write is made against.
    pub(crate) fn revision(&self) -> Option<u64> {
        self.settings.as_ref().map(|settings| settings.revision)
    }

    /// The counts `module.status` reported.
    pub(crate) fn permission_counts(&self) -> PermissionCounts {
        self.status
            .as_ref()
            .map(|status| status.permissions)
            .unwrap_or_default()
    }
}

/// Every capability-declaring module the desktop has loaded, and the one consent notice it has
/// open. At most one notice is open: a second refusal replaces it, because the operation it
/// answers has itself been replaced.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CapabilityStore {
    pub(crate) modules: BTreeMap<String, ModuleCapabilities>,
    pub(crate) consent: Option<OpenConsent>,
}

impl CapabilityStore {
    /// Some tracked job anywhere is still queued or running: the one condition the job reader
    /// exists under.
    pub(crate) fn live(&self) -> bool {
        self.modules.values().any(ModuleCapabilities::live)
    }

    /// Every live job, with its module, in a stable order.
    pub(crate) fn live_jobs(&self) -> Vec<(String, String)> {
        self.modules
            .iter()
            .flat_map(|(module, state)| {
                state
                    .jobs
                    .iter()
                    .filter(|job| !job.status.is_finished())
                    .map(|job| (module.clone(), job.job_id.as_str().to_owned()))
            })
            .collect()
    }

    /// Whether a reader's record for a tracked live job changes nothing the desktop holds: the job
    /// is still queued or running and `record` equals the one `module` already tracks, so
    /// [`ModuleCapabilities::track`] would replace it with itself. A reader's first read of a job
    /// can be one, since a round trip tracked the job just before the reader started. Compared
    /// before the record is applied. An ended job, a job not tracked and any difference (progress
    /// included) answer `false`.
    pub(crate) fn tracks_exactly(&self, module: &str, record: &JobRecord) -> bool {
        !record.status.is_finished()
            && self
                .module(module)
                .jobs
                .iter()
                .find(|held| held.job_id == record.job_id)
                == Some(record)
    }

    /// Display-only progress can replace the latest held record without deriving a hidden UI.
    /// Every lifecycle, partial result, error and subject change still takes the normal route.
    pub(crate) fn only_progress_changed(&self, module: &str, record: &JobRecord) -> bool {
        !record.status.is_finished()
            && self
                .module(module)
                .jobs
                .iter()
                .find(|held| held.job_id == record.job_id)
                .is_some_and(|held| {
                    held.kind == record.kind
                        && held.status == record.status
                        && held.asset_id == record.asset_id
                        && held.module_id == record.module_id
                        && held.resource_id == record.resource_id
                        && held.identity == record.identity
                        && held.result == record.result
                        && held.error == record.error
                        && held.first_open == record.first_open
                        && held.request_id == record.request_id
                })
    }

    /// One module's state, or the state of a module nothing has been read for.
    pub(crate) fn module(&self, module: &str) -> &ModuleCapabilities {
        self.modules.get(module).unwrap_or(&UNREAD)
    }

    /// One module's state, created on first use.
    pub(crate) fn module_mut(&mut self, module: &str) -> &mut ModuleCapabilities {
        self.modules.entry(module.to_owned()).or_default()
    }
}

// ---- models -------------------------------------------------------------------------------------

/// A module's capability block, drawn above its controls.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CapabilityModel {
    pub(crate) module_id: String,
    /// Settings and status have not been read yet.
    pub(crate) loading: bool,
    /// Nothing is in flight for this module, so its buttons may send.
    pub(crate) enabled: bool,
    pub(crate) resources: Vec<ResourceRowModel>,
    /// The module-level settings, each drawn as the tools panel draws a control of its kind.
    pub(crate) fields: Vec<FieldModel>,
    /// `2 permissions · 1 declined`, from the counts `module.status` reports.
    pub(crate) permissions: String,
    /// Some grant is live, so Revoke all has something to withdraw.
    pub(crate) revoke_all: bool,
    /// The one status line: a failure, a conflict, or settings the module cannot use.
    pub(crate) status_line: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResourceAction {
    Download,
    Cancel,
    Remove,
}

/// One resource on one row: its title, `v1 · 20 B · Installing 35%`, and the one action its state
/// allows.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResourceRowModel {
    pub(crate) id: String,
    pub(crate) title: String,
    /// Version and human size, e.g. `v1 · 20 B`.
    pub(crate) detail: String,
    /// `Not installed`, `Installing 35%`, `Installed` or `Failed: …`.
    pub(crate) state: String,
    pub(crate) progress: Option<f64>,
    pub(crate) action: ResourceAction,
    /// The install job Cancel stops.
    pub(crate) job: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FieldModel {
    pub(crate) field: String,
    /// A stable widget identity, so focus survives a redraw.
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) kind: FieldKindModel,
}

/// A setting as one of the tool panel's control kinds. Secrets and every other kind are set
/// through `module.settings.*`, never drawn.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FieldKindModel {
    /// A number or an integer: the formatted value, or the text while it is typed.
    Number {
        display: String,
        typing: Option<String>,
    },
    Toggle {
        on: bool,
    },
    Choice {
        options: Vec<String>,
        selected: Option<usize>,
    },
    /// Text or an endpoint, committed on Enter.
    Text {
        display: String,
        typing: Option<String>,
    },
}

/// The task control a module declares, with the state of its newest run on the open asset.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TaskControl {
    pub(crate) module_id: String,
    pub(crate) task: String,
    pub(crate) label: String,
    pub(crate) runnable: bool,
    pub(crate) reason: Option<String>,
    pub(crate) state: TaskControlState,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TaskControlState {
    Idle,
    Requesting,
    /// Waiting for the person's answer to the consent notice.
    Consent,
    Running {
        text: String,
        job: String,
    },
    Succeeded {
        summary: String,
        /// Apply's action and preset, when the task declares one.
        apply: Option<(String, Map<String, Value>)>,
    },
    Failed(String),
}

// ---- derivation ---------------------------------------------------------------------------------

/// The capability block of one module's section, or `None` for a module that declares nothing it
/// covers.
pub(crate) fn section(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> Option<CapabilityModel> {
    if !declares(module) {
        return None;
    }
    let state = inputs.capabilities.module(&module.id);
    let counts = state.permission_counts();
    Some(CapabilityModel {
        module_id: module.id.clone(),
        loading: state.settings.is_none() && state.status.is_none() && state.message.is_none(),
        enabled: state.pending == 0 && module.is_available(),
        resources: module
            .resources
            .iter()
            .map(|declared| resource_row(declared, state))
            .collect(),
        fields: settings_form(module, state),
        permissions: permissions_text(counts),
        revoke_all: counts.live > 0,
        status_line: status_line(state),
    })
}

/// The message the last read or operation left, else why the stored settings cannot be used.
fn status_line(state: &ModuleCapabilities) -> Option<String> {
    state.message.clone().or_else(|| {
        let read = state.settings.as_ref()?;
        (read.state == SettingsState::Incompatible).then(|| {
            format!(
                "Incompatible settings: {}",
                read.error.as_deref().unwrap_or("reset them to continue")
            )
        })
    })
}

fn progress_percent(job: Option<&JobRecord>) -> String {
    match job.and_then(|job| job.progress.fraction) {
        Some(fraction) => format!(" {:.0}%", fraction.clamp(0.0, 1.0) * 100.0),
        None => String::new(),
    }
}

/// A tracked job by id, falling back to the status's own record of it.
fn job<'a>(state: &'a ModuleCapabilities, id: &str) -> Option<&'a JobRecord> {
    state
        .jobs
        .iter()
        .find(|job| job.job_id.as_str() == id)
        .or_else(|| {
            state
                .status
                .as_ref()?
                .jobs
                .iter()
                .find(|job| job.job_id.as_str() == id)
        })
}

fn resource_row(declared: &ResourceDescriptor, state: &ModuleCapabilities) -> ResourceRowModel {
    let row = state
        .status
        .as_ref()
        .and_then(|status| status.resources.iter().find(|row| row.id == declared.id));
    let resource_state = row.map_or(ResourceState::NotInstalled, |row| row.state);
    let job_id = row
        .and_then(|row| row.job_id.as_ref())
        .map(|id| id.as_str().to_owned())
        .filter(|_| resource_state == ResourceState::Installing);
    let running = job_id.as_deref().and_then(|id| job(state, id));
    let (text, progress, action) = match resource_state {
        ResourceState::NotInstalled => ("Not installed".into(), None, ResourceAction::Download),
        ResourceState::Installing => (
            format!("Installing{}", progress_percent(running)),
            running.and_then(|job| job.progress.fraction),
            ResourceAction::Cancel,
        ),
        ResourceState::Installed => ("Installed".into(), None, ResourceAction::Remove),
        ResourceState::Failed => (
            match row.and_then(|row| row.error.as_ref()) {
                Some(error) => format!("Failed: {}", error.message),
                None => "Failed".into(),
            },
            None,
            ResourceAction::Download,
        ),
    };
    ResourceRowModel {
        id: declared.id.clone(),
        title: declared.title.clone(),
        detail: format!("v{} · {}", declared.version, human_bytes(declared.bytes)),
        state: text,
        progress,
        action,
        job: job_id,
    }
}

/// A byte count in the largest unit that keeps it at least one: `20 B`, `1.5 KB`, `2.3 GB`.
pub(crate) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn permissions_text(counts: PermissionCounts) -> String {
    let PermissionCounts {
        live,
        revoked,
        denials,
    } = counts;
    let mut text = format!("{live} permission{}", if live == 1 { "" } else { "s" });
    if revoked > 0 {
        text.push_str(&format!(" · {revoked} revoked"));
    }
    if denials > 0 {
        text.push_str(&format!(" · {denials} declined"));
    }
    text
}

/// A stable widget identity for one module-level setting.
fn field_id(module: &str, field: &str) -> String {
    format!("luxforge.setting.{module}.{field}")
}

/// The module-level settings as the tools panel's control kinds, once they have been read. A
/// number reads as a module control's number does, from the same parameter declaration.
fn settings_form(module: &ModuleDescriptor, state: &ModuleCapabilities) -> Vec<FieldModel> {
    let (Some(declared), Some(read)) = (module.settings.as_ref(), state.settings.as_ref()) else {
        return Vec::new();
    };
    declared
        .fields
        .iter()
        .filter_map(|field| {
            let value = match read.fields.get(field.id()) {
                Some(FieldRead::Value { value, .. }) => Some(value),
                _ => None,
            };
            let typing = state.edits.get(field.id()).cloned();
            let kind = match field.kind() {
                ParameterKind::Number { .. } => {
                    let spec = NumberSpec::of(&field.parameter);
                    FieldKindModel::Number {
                        display: value
                            .and_then(Value::as_f64)
                            .map(|value| {
                                spec.map_or_else(|| number_text(value), |spec| spec.format(value))
                            })
                            .unwrap_or_default(),
                        typing,
                    }
                }
                ParameterKind::Integer { .. } => FieldKindModel::Number {
                    display: value
                        .and_then(Value::as_i64)
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                    typing,
                },
                ParameterKind::Boolean => FieldKindModel::Toggle {
                    on: value.and_then(Value::as_bool).unwrap_or(false),
                },
                ParameterKind::Enum { options } => FieldKindModel::Choice {
                    selected: value
                        .and_then(Value::as_str)
                        .and_then(|chosen| options.iter().position(|option| option == chosen)),
                    options: options.clone(),
                },
                ParameterKind::String { .. } | ParameterKind::Endpoint { .. } => {
                    FieldKindModel::Text {
                        display: value.and_then(Value::as_str).unwrap_or_default().to_owned(),
                        typing,
                    }
                }
                _ => return None,
            };
            Some(FieldModel {
                field: field.id().to_owned(),
                id: field_id(&module.id, field.id()),
                label: field.label.clone(),
                kind,
            })
        })
        .collect()
}

/// The open asset's current recipe references this artifact.
fn applied(open: Option<&EditorState>, artifact: &str) -> bool {
    open.is_some_and(|open| {
        open.current_entry
            .snapshot
            .recipe
            .layers
            .iter()
            .any(|layer| layer.artifacts.iter().any(|id| id.as_str() == artifact))
    })
}

/// A disclosure line starts with a capital, whatever the module wrote.
fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

fn short(value: &str) -> &str {
    value.get(..value.len().min(14)).unwrap_or(value)
}

/// The task control of one declared `task` control: the button and the state of its newest run on
/// the open asset.
pub(crate) fn task_control(
    module: &ModuleDescriptor,
    task: &str,
    label: &str,
    inputs: &Inputs<'_>,
    enabled: bool,
) -> TaskControl {
    let state = inputs.capabilities.module(&module.id);
    let declared = module.task(task);
    let asset = inputs.document.state.as_ref().map(|state| &state.asset.id);
    let run = state
        .tasks
        .get(task)
        .filter(|run| run.asset.is_none() || run.asset.as_ref() == asset);
    let consent_open = inputs
        .capabilities
        .consent
        .as_ref()
        .is_some_and(|open| open.consent.module_id == module.id);
    let state_model = match run.map(|run| &run.phase) {
        None => TaskControlState::Idle,
        Some(TaskPhase::Requesting) => TaskControlState::Requesting,
        Some(TaskPhase::Consent) if consent_open => TaskControlState::Consent,
        Some(TaskPhase::Consent) => TaskControlState::Idle,
        Some(TaskPhase::Job(id)) => {
            let record = job(state, id);
            TaskControlState::Running {
                text: format!(
                    "{}{}{}",
                    match record.map(|job| job.status) {
                        Some(JobStatus::Queued) => "Queued",
                        _ => "Running",
                    },
                    progress_percent(record),
                    record
                        .and_then(|job| job.progress.message.as_deref())
                        .map(|message| format!(" · {message}"))
                        .unwrap_or_default()
                ),
                job: id.clone(),
            }
        }
        Some(TaskPhase::Succeeded { artifacts, .. }) => {
            // Once the current recipe references the result, Apply would change nothing.
            let applied = artifacts
                .first()
                .is_some_and(|artifact| applied(inputs.document.state.as_ref(), artifact));
            TaskControlState::Succeeded {
                summary: match (artifacts.first(), applied) {
                    (Some(artifact), true) => format!("Applied · {}", short(artifact)),
                    (Some(artifact), false) => format!("Done · {}", short(artifact)),
                    (None, _) => "Done".into(),
                },
                apply: declared
                    .and_then(|task| task.apply.as_ref())
                    .zip(artifacts.first())
                    .filter(|_| !applied)
                    .map(|(apply, artifact)| {
                        (
                            apply.action.clone(),
                            [(apply.parameter.clone(), Value::from(artifact.clone()))]
                                .into_iter()
                                .collect(),
                        )
                    }),
            }
        }
        Some(TaskPhase::Failed { code, message }) => {
            TaskControlState::Failed(format!("{code}: {message}"))
        }
    };
    let takes_profile = declared.is_some_and(|task| task.profile);
    let reason = if declared.is_none() {
        Some(format!("{} declares no task {task}", module.title))
    } else if declared.is_some_and(|task| task.asset) && inputs.document.state.is_none() {
        Some("No photograph is open".into())
    } else if takes_profile && task_profile(state, takes_profile).is_none() {
        let label = module
            .settings
            .as_ref()
            .and_then(|settings| settings.profiles.as_ref())
            .map_or("profile", |profiles| profiles.label.as_str());
        Some(format!(
            "No {label} profile yet: create one with module.profile.create"
        ))
    } else if matches!(
        state_model,
        TaskControlState::Requesting | TaskControlState::Running { .. } | TaskControlState::Consent
    ) {
        Some("The task is already running".into())
    } else {
        None
    };
    TaskControl {
        module_id: module.id.clone(),
        task: task.to_owned(),
        label: label.to_owned(),
        runnable: enabled && state.pending == 0 && reason.is_none(),
        reason,
        state: state_model,
    }
}

/// The profile a task run sends: the first ready one, else the first profile so the core can say
/// what it lacks. `None` when the task takes no profile or none exists. Another profile is chosen
/// by sending `task.<id>` with its `profile_id`.
pub(crate) fn task_profile(state: &ModuleCapabilities, takes_profile: bool) -> Option<String> {
    if !takes_profile {
        return None;
    }
    let profiles = state
        .settings
        .as_ref()
        .map(|settings| settings.profiles.as_slice())
        .unwrap_or_default();
    profiles
        .iter()
        .find(|profile| profile.status == ProfileStatus::Ready)
        .or_else(|| profiles.first())
        .map(|profile| profile.id.clone())
}

/// The consent notice over the canvas, when a capability operation the desktop started was refused
/// for want of consent. It is not modal: the rest of the workspace stays usable.
pub(crate) fn consent_notice(inputs: &Inputs<'_>) -> Option<Notice> {
    let open = inputs.capabilities.consent.as_ref()?;
    let consent = &open.consent;
    let disclosure = &consent.disclosure;
    let verb = match consent.kind {
        GrantKind::DownloadArtifact => "download a resource",
        GrantKind::RemoteImageRequest => "send photo data",
    };
    let mut lines = vec![disclosure.purpose.clone()];
    if let Some(destination) = &disclosure.destination {
        lines.push(match consent.kind {
            GrantKind::DownloadArtifact => format!("From {destination}"),
            _ => format!("To {destination}"),
        });
    }
    let data = capitalised(&disclosure.data);
    lines.push(match disclosure.bytes {
        Some(bytes) => format!("{data} · {}", human_bytes(bytes)),
        None => data,
    });
    if let Some(storage) = &disclosure.storage {
        lines.push(format!("Stored in {storage}"));
    }
    if let Some(license) = &disclosure.license {
        lines.push(format!("License {license}"));
    }
    if let Some(retention) = &disclosure.retention {
        lines.push(format!("Retention: {retention}"));
    }
    lines.push(
        match disclosure.cost {
            AdapterCost::Free => "Free",
            AdapterCost::Paid => "May cost money",
            AdapterCost::Unknown => "Cost unknown",
        }
        .into(),
    );
    if consent.denied {
        lines.push("You declined this before.".into());
    }
    Some(Notice {
        tone: NoticeTone::Warning,
        icon: NoticeIcon::Triangle,
        title: format!("Allow {} to {verb}?", disclosure.module),
        body: lines.join("\n"),
        actions: vec![
            ("Allow".into(), NoticeAction::AllowConsent),
            ("Don't allow".into(), NoticeAction::DenyConsent),
        ],
    })
}

// ---- evidence -----------------------------------------------------------------------------------

/// What a captured frame's state reports for every capability-declaring module: the settings as
/// the core read them (a secret only as whether it is present), each resource, the jobs, each
/// task's newest run on the open asset, the permission counts, the open consent notice and the
/// status line. It never holds a secret: none reaches this store.
pub(crate) fn summary(
    store: &CapabilityStore,
    modules: &[ModuleDescriptor],
    open: Option<&EditorState>,
) -> Value {
    Value::Object(
        modules
            .iter()
            .filter(|module| declares(module))
            .map(|module| (module.id.clone(), module_summary(module, store, open)))
            .collect(),
    )
}

fn module_summary(
    module: &ModuleDescriptor,
    store: &CapabilityStore,
    open: Option<&EditorState>,
) -> Value {
    let state = store.module(&module.id);
    let asset = open.map(|open| &open.asset.id);
    let resources: Vec<Value> = module
        .resources
        .iter()
        .map(|declared| {
            let row = resource_row(declared, state);
            json!({
                "id": row.id,
                "state": state
                    .status
                    .as_ref()
                    .and_then(|status| status.resources.iter().find(|listed| listed.id == row.id))
                    .map_or(ResourceState::NotInstalled, |listed| listed.state),
                "progress": row.progress,
                "text": row.state,
            })
        })
        .collect();
    let jobs: Vec<Value> = state
        .jobs
        .iter()
        .map(|job| {
            json!({
                "job_id": job.job_id,
                "kind": job.kind,
                "status": job.status,
                "progress": job.progress,
                "error": job.error,
            })
        })
        .collect();
    let tasks: Map<String, Value> = state
        .tasks
        .iter()
        .filter(|(_, run)| run.asset.is_none() || run.asset.as_ref() == asset)
        .map(|(task, run)| {
            let applies = module.task(task).and_then(|task| task.apply.as_ref());
            let mut summary = match &run.phase {
                TaskPhase::Requesting => json!({"status": "requesting"}),
                TaskPhase::Consent => json!({"status": "consent"}),
                TaskPhase::Job(id) => {
                    let record = job(state, id);
                    json!({
                        "status": record.map(|job| job.status),
                        "job_id": id,
                        "progress": record.map(|job| &job.progress),
                    })
                }
                TaskPhase::Succeeded {
                    job,
                    artifacts,
                    result,
                } => json!({
                    // The shared job vocabulary's word for success, so a task's summary reads the
                    // same status word whether it is still in flight (`TaskPhase::Job` passes the
                    // capability job's own `status` through) or has settled here.
                    "status": "ready",
                    "job_id": job,
                    "artifact": artifacts.first(),
                    "artifacts": artifacts,
                    "apply_available": applies.is_some() && !artifacts.is_empty(),
                    "applied": artifacts.first().is_some_and(|artifact| applied(open, artifact)),
                    "result": result,
                }),
                TaskPhase::Failed { code, message } => json!({
                    "status": "failed",
                    "error": {"code": code, "message": message},
                }),
            };
            summary["asset_id"] = json!(run.asset);
            summary["profile_id"] = json!(run.profile);
            (task.clone(), summary)
        })
        .collect();
    let counts = state.permission_counts();
    let consent = store
        .consent
        .as_ref()
        .filter(|open| open.consent.module_id == module.id)
        .map(|open| {
            json!({
                "capability": open.consent.capability,
                "kind": open.consent.kind,
                "scope": open.consent.scope,
                "denied": open.consent.denied,
                "retry": open.retry.as_ref().map(Operation::name),
            })
        });
    json!({
        "loaded": state.settings.is_some() || state.status.is_some(),
        "pending": state.pending,
        "settings": state.settings,
        "resources": resources,
        "jobs": jobs,
        "tasks": tasks,
        "permissions": {
            "live": counts.live,
            "revoked": counts.revoked,
            "denials": counts.denials,
        },
        "consent": consent,
        "status_line": status_line(state),
    })
}
