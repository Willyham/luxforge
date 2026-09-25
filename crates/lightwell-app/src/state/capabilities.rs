//! Module capabilities as the desktop knows them, and the models its tools panel, notices and
//! evidence are derived from. Everything here is plain data: the owner's own answers
//! (`module.settings.read`, `module.status`, `module.job.read`, a `consent-required` failure), the
//! text a person is typing, and which sub-view of a section is open. Nothing here calls the owner,
//! and nothing decides what the host accepts: every value is validated by the core when it is sent.
//! The section is generated from the descriptor, so any module that declares settings, resources,
//! an activation or tasks gets the same surface.
use crate::state::{
    Inputs,
    canvas::{Notice, NoticeAction, NoticeTone},
};
use lightwell_core::{
    AssetId, EditorState, ModuleDescriptor, ParameterKind,
    capabilities::{
        consent::Disclosure,
        descriptor::{AdapterCost, SettingDescriptor},
        grants::{Grant, GrantKind, GrantList},
        host::{ActivationRead, ActivationState, Requirement},
        jobs::{JobRecord, JobStatus},
        resources::{ResourceRow, ResourceState},
        settings::{FieldRead, ProfileStatus, SettingsRead, SettingsState},
        transport::{EndpointClass, parse_endpoint},
    },
};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroizing;

/// The most jobs one module's state keeps; finished ones go first.
const MAX_TRACKED: usize = 16;

/// A module the capability surface applies to: it declares settings, resources, an activation or
/// tasks.
pub(crate) fn declares(module: &ModuleDescriptor) -> bool {
    module.settings.is_some()
        || !module.resources.is_empty()
        || module.activation.is_some()
        || !module.tasks.is_empty()
}

/// Text typed into a secret's masked Replace field. It is zeroed when dropped, prints as
/// `<redacted>`, is never serialized, and leaves the desktop only as the `value` of one
/// `module.settings.set-secret` request.
#[derive(Clone, Default)]
pub(crate) struct SecretText(Zeroizing<String>);

impl SecretText {
    pub(crate) fn new(text: String) -> Self {
        Self(Zeroizing::new(text))
    }

    /// The typed text, for the masked field that shows it as dots and for the one request that
    /// sends it.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(lightwell_core::capabilities::redact::REDACTED)
    }
}

impl PartialEq for SecretText {
    fn eq(&self, other: &Self) -> bool {
        self.expose() == other.expose()
    }
}

/// `module.status` as the desktop reads it: the activation, every declared resource, the grants and
/// denials, and the module's recent jobs. The settings summary it also carries is read in full
/// through `module.settings.read` instead.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub(crate) struct ModuleStatus {
    pub(crate) activation: ActivationRead,
    #[serde(default)]
    pub(crate) resources: Vec<ResourceRow>,
    #[serde(default)]
    pub(crate) permissions: GrantList,
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

/// One settings field: `(profile id, setting id)`, with no profile for a module-level field.
pub(crate) type FieldKey = (Option<String>, String);

/// Which sub-view a capability section shows above its controls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum CapabilityView {
    #[default]
    Status,
    Settings,
}

impl CapabilityView {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Settings => "settings",
        }
    }
}

/// One operation the desktop asks the owner for, as plain data: the app layer turns it into the
/// owner requests an independent JSON client sends. Settings writes carry the settings revision
/// they were made against, so a stale one is refused as a conflict rather than applied.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Operation {
    /// Read the module's settings and status.
    Load,
    /// Read the module's status alone.
    Refresh,
    Set {
        profile: Option<String>,
        field: String,
        value: Value,
        revision: u64,
    },
    SetSecret {
        profile: Option<String>,
        field: String,
        value: SecretText,
        revision: u64,
    },
    ClearSecret {
        profile: Option<String>,
        field: String,
        revision: u64,
    },
    CreateProfile {
        adapter: String,
        label: String,
        revision: u64,
    },
    RemoveProfile {
        profile: String,
        revision: u64,
    },
    Install {
        resource: String,
    },
    Remove {
        resource: String,
    },
    Activate,
    Deactivate,
    Cancel {
        job: String,
    },
    Revoke {
        grant: String,
    },
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
            Self::SetSecret { .. } => "set-secret",
            Self::ClearSecret { .. } => "clear-secret",
            Self::CreateProfile { .. } => "create-profile",
            Self::RemoveProfile { .. } => "remove-profile",
            Self::Install { .. } => "install",
            Self::Remove { .. } => "remove",
            Self::Activate => "activate",
            Self::Deactivate => "deactivate",
            Self::Cancel { .. } => "cancel",
            Self::Revoke { .. } => "revoke",
            Self::RunTask { .. } => "task",
            Self::Consent { allow: true, .. } => "allow",
            Self::Consent { allow: false, .. } => "deny",
        }
    }

    /// The settings field a write is about, so its error or conflict is shown under that field.
    pub(crate) fn field(&self) -> Option<FieldKey> {
        match self {
            Self::Set { profile, field, .. }
            | Self::SetSecret { profile, field, .. }
            | Self::ClearSecret { profile, field, .. } => Some((profile.clone(), field.clone())),
            _ => None,
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
    /// The core answered `not-ready`; the requirements are the module's.
    NotReady,
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
    /// Why the last read failed, when it did.
    pub(crate) load_error: Option<String>,
    pub(crate) view: CapabilityView,
    pub(crate) permissions_open: bool,
    /// Text being typed into a field, until it is committed or abandoned.
    pub(crate) edits: BTreeMap<FieldKey, String>,
    /// The secret whose masked Replace field is open, with what has been typed into it.
    pub(crate) secret: Option<(FieldKey, SecretText)>,
    /// The core's refusal of the last write to a field, shown under it.
    pub(crate) errors: BTreeMap<FieldKey, String>,
    /// Fields whose last write met a newer settings revision: re-read, and marked.
    pub(crate) conflicts: BTreeSet<FieldKey>,
    /// What the last `not-ready` answer said is missing.
    pub(crate) requirements: Vec<Requirement>,
    /// The last operation's failure that belongs to no field.
    pub(crate) message: Option<String>,
    /// The capability jobs the desktop started and the module's live ones, oldest first.
    pub(crate) jobs: Vec<JobRecord>,
    /// The newest run of each task, by task id.
    pub(crate) tasks: BTreeMap<String, TaskRun>,
    /// The profile a person chose for each task, when several are ready.
    pub(crate) task_profiles: BTreeMap<String, String>,
    /// The Add profile form.
    pub(crate) profile_adapter: Option<String>,
    pub(crate) profile_label: String,
    /// Owner requests in flight for this module.
    pub(crate) pending: u32,
    /// Increases on every change, so the module's section is re-derived exactly when this does.
    pub(crate) version: u64,
}

impl ModuleCapabilities {
    /// Some tracked job is still queued or running.
    pub(crate) fn live(&self) -> bool {
        self.jobs.iter().any(|job| !job.status.is_finished())
    }

    /// The newest tracked job still queued or running.
    pub(crate) fn newest_live(&self) -> Option<&JobRecord> {
        self.jobs.iter().rev().find(|job| !job.status.is_finished())
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

    /// The profile identity at a position of the settings read's list.
    pub(crate) fn profile_at(&self, index: usize) -> Option<String> {
        self.settings
            .as_ref()
            .and_then(|settings| settings.profiles.get(index))
            .map(|profile| profile.id.clone())
    }

    /// Every grant as the permissions list shows it, live ones first in the order they were made.
    pub(crate) fn grants(&self) -> Vec<&Grant> {
        let mut grants: Vec<&Grant> = self
            .status
            .iter()
            .flat_map(|status| status.permissions.grants.iter())
            .collect();
        grants.sort_by_key(|grant| grant.revoked.is_some());
        grants
    }
}

/// Every capability-declaring module the desktop has loaded, and the one consent notice it has
/// open. At most one notice is open: a second refusal replaces it, because the operation it
/// answers has itself been replaced.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CapabilityStore {
    pub(crate) modules: BTreeMap<String, ModuleCapabilities>,
    pub(crate) consent: Option<OpenConsent>,
    /// One `module.job.read` batch is in flight.
    pub(crate) polling: bool,
}

impl CapabilityStore {
    /// Some tracked job anywhere is still queued or running: the one condition the job poll exists
    /// under.
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

    /// One module's state, created on first use, with its version bumped: every caller changes it.
    pub(crate) fn touch(&mut self, module: &str) -> &mut ModuleCapabilities {
        let state = self.modules.entry(module.to_owned()).or_default();
        state.version += 1;
        state
    }
}

// ---- models -------------------------------------------------------------------------------------

/// A module's capability block, drawn above its controls.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CapabilityModel {
    pub(crate) module_id: String,
    pub(crate) view: CapabilityView,
    /// Settings and status have not been read yet.
    pub(crate) loading: bool,
    /// Nothing is in flight for this module, so its buttons may send.
    pub(crate) enabled: bool,
    /// A read failed or the last operation failed with no field to blame.
    pub(crate) message: Option<String>,
    pub(crate) activation: Option<ActivationRow>,
    pub(crate) resources: Vec<ResourceRowModel>,
    pub(crate) permissions: PermissionsModel,
    /// What a `not-ready` answer said is missing, by name.
    pub(crate) requirements: Vec<String>,
    pub(crate) settings: Option<SettingsModel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ActivationRow {
    /// `Inactive`, `Activating 40%`, `Active` or `Failed: <reason>`.
    pub(crate) text: String,
    /// Activate while inactive or failed, Deactivate while activating or active.
    pub(crate) activate: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ResourceAction {
    Download,
    Cancel,
    Remove,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ResourceRowModel {
    pub(crate) id: String,
    pub(crate) title: String,
    /// Version and human size, e.g. `v1 · 20 B`.
    pub(crate) detail: String,
    /// `Not installed`, `Installing 35%`, `Installed` or `Failed: …`.
    pub(crate) state: String,
    pub(crate) progress: Option<f64>,
    pub(crate) actions: Vec<ResourceAction>,
    /// The install job Cancel stops.
    pub(crate) job: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PermissionsModel {
    /// `2 permissions · 1 declined`.
    pub(crate) summary: String,
    pub(crate) open: bool,
    pub(crate) rows: Vec<PermissionRow>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PermissionRow {
    pub(crate) text: String,
    /// `Allowed`, `Revoked` or `Declined`.
    pub(crate) state: String,
    /// The grant Revoke withdraws, for a live grant.
    pub(crate) revoke: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SettingsModel {
    /// Why the stored settings cannot be used, when they cannot.
    pub(crate) state: Option<String>,
    pub(crate) fields: Vec<FieldModel>,
    /// The profile block's declared label, when the module declares profiles.
    pub(crate) profiles_label: Option<String>,
    pub(crate) profiles: Vec<ProfileModel>,
    pub(crate) add_profile: Option<AddProfileModel>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProfileModel {
    pub(crate) id: String,
    /// `Local proof · Proof echo · ready`.
    pub(crate) title: String,
    pub(crate) status: String,
    pub(crate) fields: Vec<FieldModel>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AddProfileModel {
    /// `(adapter id, title)`; a choice is shown only when there is more than one.
    pub(crate) adapters: Vec<(String, String)>,
    pub(crate) selected: usize,
    pub(crate) label: String,
    pub(crate) can_add: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FieldModel {
    pub(crate) profile: Option<String>,
    pub(crate) field: String,
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) help: Option<String>,
    pub(crate) kind: FieldKindModel,
    /// The core's refusal, or why a stored value is not valid.
    pub(crate) error: Option<String>,
    /// A write met a newer revision; the field shows the re-read value.
    pub(crate) conflict: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum FieldKindModel {
    /// A number or an integer: the formatted value, or the text while it is typed, and the
    /// declared range for the hint under an invalid entry.
    Number {
        display: String,
        typing: Option<String>,
        range: String,
    },
    Toggle {
        on: bool,
    },
    Choice {
        options: Vec<String>,
        selected: Option<usize>,
    },
    /// Text or an endpoint, committed on Enter; an endpoint names its class.
    Text {
        display: String,
        typing: Option<String>,
        class: Option<String>,
    },
    /// Only `Set`, `Not set` or `Unknown`, and the masked input while replacing.
    Secret {
        state: String,
        replacing: Option<SecretText>,
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
    /// Ready profiles to choose between, when more than one is ready.
    pub(crate) profiles: Vec<(String, String)>,
    pub(crate) profile: Option<String>,
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
    NotReady,
}

// ---- derivation ---------------------------------------------------------------------------------

/// The capability block of one module's section, or `None` for a module that declares nothing it
/// covers.
pub(crate) fn section(module: &ModuleDescriptor, inputs: &Inputs<'_>) -> Option<CapabilityModel> {
    if !declares(module) {
        return None;
    }
    let empty = ModuleCapabilities::default();
    let state = inputs
        .capabilities
        .modules
        .get(&module.id)
        .unwrap_or(&empty);
    let status = state.status.as_ref();
    let activation = module.activation.as_ref().map(|_| {
        let read = status.map(|status| &status.activation);
        ActivationRow {
            text: read
                .map(|read| activation_text(read, state))
                .unwrap_or_else(|| "Inactive".into()),
            activate: !read.is_some_and(|read| {
                matches!(
                    read.state,
                    ActivationState::Active | ActivationState::Activating
                )
            }),
        }
    });
    let resources = module
        .resources
        .iter()
        .map(|declared| {
            let row =
                status.and_then(|status| status.resources.iter().find(|row| row.id == declared.id));
            resource_row(declared, row, state)
        })
        .collect();
    Some(CapabilityModel {
        module_id: module.id.clone(),
        view: state.view,
        loading: state.settings.is_none() && state.status.is_none() && state.load_error.is_none(),
        enabled: state.pending == 0 && module.is_available(),
        message: state.load_error.clone().or_else(|| state.message.clone()),
        activation,
        resources,
        permissions: permissions(module, state),
        requirements: state
            .requirements
            .iter()
            .map(|requirement| requirement_text(module, state, requirement))
            .collect(),
        settings: (state.view == CapabilityView::Settings)
            .then(|| settings_model(module, state))
            .flatten(),
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

fn activation_text(read: &ActivationRead, state: &ModuleCapabilities) -> String {
    match read.state {
        ActivationState::Inactive => match &read.reason {
            Some(reason) => format!("Inactive · {reason}"),
            None => "Inactive".into(),
        },
        ActivationState::Activating => {
            let job = read.job_id.as_ref().and_then(|id| job(state, id.as_str()));
            format!("Activating{}", progress_percent(job))
        }
        ActivationState::Active => "Active".into(),
        ActivationState::Failed => match &read.error {
            Some(error) => format!("Failed: {}", error.message),
            None => "Failed".into(),
        },
    }
}

fn resource_row(
    declared: &lightwell_core::capabilities::descriptor::ResourceDescriptor,
    row: Option<&ResourceRow>,
    state: &ModuleCapabilities,
) -> ResourceRowModel {
    let resource_state = row.map_or(ResourceState::NotInstalled, |row| row.state);
    let job_id = row
        .and_then(|row| row.job_id.as_ref())
        .map(|id| id.as_str().to_owned());
    let running = job_id.as_deref().and_then(|id| job(state, id));
    let (text, progress, actions) = match resource_state {
        ResourceState::NotInstalled => {
            ("Not installed".into(), None, vec![ResourceAction::Download])
        }
        ResourceState::Installing => (
            format!("Installing{}", progress_percent(running)),
            running.and_then(|job| job.progress.fraction),
            vec![ResourceAction::Cancel],
        ),
        ResourceState::Installed => ("Installed".into(), None, vec![ResourceAction::Remove]),
        ResourceState::Failed => (
            match row.and_then(|row| row.error.as_ref()) {
                Some(error) => format!("Failed: {}", error.message),
                None => "Failed".into(),
            },
            None,
            vec![ResourceAction::Download],
        ),
    };
    ResourceRowModel {
        id: declared.id.clone(),
        title: declared.title.clone(),
        detail: format!("v{} · {}", declared.version, human_bytes(declared.bytes)),
        state: text,
        progress,
        actions,
        job: job_id.filter(|_| resource_state == ResourceState::Installing),
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

fn permissions(module: &ModuleDescriptor, state: &ModuleCapabilities) -> PermissionsModel {
    let grants = state.grants();
    let live = grants.iter().filter(|grant| grant.is_live()).count();
    let revoked = grants.len() - live;
    let denials = state
        .status
        .as_ref()
        .map(|status| status.permissions.denials.as_slice())
        .unwrap_or_default();
    let mut summary = format!("{live} permission{}", if live == 1 { "" } else { "s" });
    if revoked > 0 {
        summary.push_str(&format!(" · {revoked} revoked"));
    }
    if !denials.is_empty() {
        summary.push_str(&format!(" · {} declined", denials.len()));
    }
    let mut rows: Vec<PermissionRow> = grants
        .iter()
        .map(|grant| PermissionRow {
            text: scope_text(
                module,
                state,
                &grant.capability,
                grant.kind,
                &scope_value(grant),
            ),
            state: if grant.is_live() {
                "Allowed"
            } else {
                "Revoked"
            }
            .into(),
            revoke: grant.is_live().then(|| grant.grant_id.clone()),
        })
        .collect();
    rows.extend(denials.iter().map(|denial| PermissionRow {
        text: scope_text(
            module,
            state,
            &denial.capability,
            denial.kind,
            &serde_json::to_value(&denial.scope).unwrap_or(Value::Null),
        ),
        state: "Declined".into(),
        revoke: None,
    }));
    PermissionsModel {
        summary,
        open: state.permissions_open,
        rows,
    }
}

fn scope_value(grant: &Grant) -> Value {
    serde_json::to_value(&grant.scope).unwrap_or(Value::Null)
}

/// One scope in words: what it lets the module do, and to or from where.
fn scope_text(
    module: &ModuleDescriptor,
    state: &ModuleCapabilities,
    capability: &str,
    kind: GrantKind,
    scope: &Value,
) -> String {
    let text = |key: &str| scope.get(key).and_then(Value::as_str).unwrap_or_default();
    let described = match kind {
        GrantKind::DownloadArtifact => {
            let title = module
                .resource(text("resource"))
                .map_or(text("resource"), |resource| resource.title.as_str());
            format!(
                "Download {title} {} from {}",
                text("version"),
                text("origin")
            )
        }
        GrantKind::RemoteImageRequest => {
            let profile = state
                .settings
                .as_ref()
                .and_then(|settings| settings.profile(text("profile_id")))
                .map_or(text("profile_id"), |profile| profile.label.as_str());
            format!(
                "Send {} of {} to {} ({profile})",
                text("data"),
                short(text("asset_id")),
                text("origin")
            )
        }
    };
    format!("{} · {capability}", described.trim())
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

/// One requirement by name: the setting's label, the resource's title, the profile's label.
fn requirement_text(
    module: &ModuleDescriptor,
    state: &ModuleCapabilities,
    requirement: &Requirement,
) -> String {
    let name = match requirement.kind.as_str() {
        "setting" => module
            .settings
            .as_ref()
            .and_then(|settings| settings.field(&requirement.id))
            .map(|field| field.label.clone()),
        "resource" => module
            .resource(&requirement.id)
            .map(|resource| resource.title.clone()),
        "profile" => state
            .settings
            .as_ref()
            .and_then(|settings| settings.profile(&requirement.id))
            .map(|profile| format!("Profile {}", profile.label)),
        "activation" => Some("Activation".into()),
        "grant" => module
            .capability(&requirement.id)
            .map(|capability| format!("Permission {}", capability.id)),
        _ => None,
    }
    .unwrap_or_else(|| requirement.id.clone());
    format!("{name}: {}", requirement.state.replace('-', " "))
}

fn settings_model(module: &ModuleDescriptor, state: &ModuleCapabilities) -> Option<SettingsModel> {
    let declared = module.settings.as_ref()?;
    let read = state.settings.as_ref();
    let fields = declared
        .fields
        .iter()
        .map(|field| {
            field_model(
                &module.id,
                None,
                field,
                read.and_then(|read| read.fields.get(field.id())),
                state,
            )
        })
        .collect();
    let profiles_declared = declared.profiles.as_ref();
    let profiles = read
        .map(|read| read.profiles.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|profile| {
            let adapter = profiles_declared
                .and_then(|profiles| profiles.adapter(&profile.adapter))
                .map_or(profile.adapter.as_str(), |adapter| adapter.title.as_str());
            let status = profile_status(profile.status);
            ProfileModel {
                id: profile.id.clone(),
                title: format!("{} · {adapter} · {status}", profile.label),
                status: status.into(),
                fields: profiles_declared
                    .map(|profiles| profiles.fields.as_slice())
                    .unwrap_or_default()
                    .iter()
                    .map(|field| {
                        field_model(
                            &module.id,
                            Some(&profile.id),
                            field,
                            profile.fields.get(field.id()),
                            state,
                        )
                    })
                    .collect(),
            }
        })
        .collect::<Vec<_>>();
    let add_profile = profiles_declared.map(|declared| {
        let adapters: Vec<(String, String)> = declared
            .adapters
            .iter()
            .map(|adapter| (adapter.id.clone(), adapter.title.clone()))
            .collect();
        let selected = state
            .profile_adapter
            .as_ref()
            .and_then(|chosen| adapters.iter().position(|(id, _)| id == chosen))
            .unwrap_or(0);
        AddProfileModel {
            can_add: !state.profile_label.trim().is_empty()
                && profiles.len() < usize::from(declared.max)
                && !adapters.is_empty()
                && read.is_some(),
            adapters,
            selected,
            label: state.profile_label.clone(),
        }
    });
    Some(SettingsModel {
        state: read.and_then(|read| match read.state {
            SettingsState::Incompatible => Some(format!(
                "Incompatible settings: {}",
                read.error.as_deref().unwrap_or("reset them to continue")
            )),
            SettingsState::Ready | SettingsState::Incomplete => None,
        }),
        fields,
        profiles_label: profiles_declared.map(|profiles| format!("{} profiles", profiles.label)),
        profiles,
        add_profile,
    })
}

pub(crate) fn profile_status(status: ProfileStatus) -> &'static str {
    match status {
        ProfileStatus::Ready => "ready",
        ProfileStatus::Incomplete => "incomplete",
        ProfileStatus::MissingCredentials => "missing credentials",
        ProfileStatus::Incompatible => "incompatible",
    }
}

/// A stable widget identity for one settings field, so focus survives a redraw.
pub(crate) fn field_id(module: &str, profile: Option<&str>, field: &str) -> String {
    format!(
        "lightwell.setting.{module}.{}.{field}",
        profile.unwrap_or("-")
    )
}

fn field_model(
    module: &str,
    profile: Option<&String>,
    declared: &SettingDescriptor,
    read: Option<&FieldRead>,
    state: &ModuleCapabilities,
) -> FieldModel {
    let key: FieldKey = (profile.cloned(), declared.id().to_owned());
    let typing = state.edits.get(&key).cloned();
    let (value, read_error) = match read {
        Some(FieldRead::Value { value, error, .. }) => (Some(value), error.clone()),
        Some(FieldRead::Secret { error, .. }) => (None, error.clone()),
        None => (None, None),
    };
    // A number reads as a module control's number does, from the same parameter declaration.
    let spec = crate::state::number::NumberSpec::of(&declared.parameter);
    let number = |value: f64| {
        spec.map_or_else(
            || crate::state::number::number_text(value),
            |spec| spec.format(value),
        )
    };
    let kind = match declared.kind() {
        ParameterKind::Number { min, max } => FieldKindModel::Number {
            display: value
                .and_then(Value::as_f64)
                .map(number)
                .unwrap_or_default(),
            typing,
            range: format!("{} to {}", number(*min), number(*max)),
        },
        ParameterKind::Integer { min, max } => FieldKindModel::Number {
            display: value
                .and_then(Value::as_i64)
                .map(|value| value.to_string())
                .unwrap_or_default(),
            typing,
            range: format!("{min} to {max}"),
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
        ParameterKind::String { .. } => FieldKindModel::Text {
            display: value.and_then(Value::as_str).unwrap_or_default().to_owned(),
            typing,
            class: None,
        },
        ParameterKind::Endpoint { classes } => {
            let text = value.and_then(Value::as_str).unwrap_or_default();
            FieldKindModel::Text {
                display: text.to_owned(),
                typing,
                class: parse_endpoint(text, classes).ok().map(|endpoint| {
                    match endpoint.class {
                        EndpointClass::Loopback => "loopback",
                        EndpointClass::Remote => "remote",
                    }
                    .to_owned()
                }),
            }
        }
        ParameterKind::Secret { .. } => FieldKindModel::Secret {
            state: secret_state(read).into(),
            replacing: state
                .secret
                .as_ref()
                .filter(|(open, _)| *open == key)
                .map(|(_, text)| text.clone()),
        },
        // Registration refuses every other kind for a setting, so none reaches a settings view;
        // were one to, it would show its value and commit nothing.
        ParameterKind::Color
        | ParameterKind::Points { .. }
        | ParameterKind::Artifact
        | ParameterKind::Curve { .. }
        | ParameterKind::Settings
        | ParameterKind::Identity { .. } => FieldKindModel::Text {
            display: value.map(Value::to_string).unwrap_or_default(),
            typing,
            class: None,
        },
    };
    FieldModel {
        profile: profile.cloned(),
        field: declared.id().to_owned(),
        id: field_id(module, profile.map(String::as_str), declared.id()),
        label: declared.label.clone(),
        help: Some(declared.parameter.notes.clone()).filter(|notes| !notes.is_empty()),
        kind,
        error: state.errors.get(&key).cloned().or(read_error),
        conflict: state.conflicts.contains(&key),
    }
}

/// A secret as a client may see it: whether it is set, never what it is.
fn secret_state(read: Option<&FieldRead>) -> &'static str {
    match read {
        Some(FieldRead::Secret {
            secret_present: Some(true),
            ..
        }) => "Set",
        Some(FieldRead::Secret {
            secret_present: Some(false),
            ..
        }) => "Not set",
        _ => "Unknown",
    }
}

/// A number setting's value with the decimals its declared precision, else its step, gives.
/// The task control of one declared `task` control: the button, the profile it would send and the
/// state of its newest run on the open asset.
pub(crate) fn task_control(
    module: &ModuleDescriptor,
    task: &str,
    label: &str,
    inputs: &Inputs<'_>,
    enabled: bool,
) -> TaskControl {
    let empty = ModuleCapabilities::default();
    let state = inputs
        .capabilities
        .modules
        .get(&module.id)
        .unwrap_or(&empty);
    let declared = module.task(task);
    let settings = state.settings.as_ref();
    let ready: Vec<(String, String)> = settings
        .map(|settings| settings.profiles.as_slice())
        .unwrap_or_default()
        .iter()
        .filter(|profile| profile.status == ProfileStatus::Ready)
        .map(|profile| (profile.id.clone(), profile.label.clone()))
        .collect();
    let profile = task_profile(state, task, declared.is_some_and(|task| task.profile));
    let asset = inputs.state.map(|state| &state.asset.id);
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
                .is_some_and(|artifact| applied(inputs.state, artifact));
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
        Some(TaskPhase::NotReady) => TaskControlState::NotReady,
    };
    let reason = if declared.is_none() {
        Some(format!("{} declares no task {task}", module.title))
    } else if declared.is_some_and(|task| task.asset) && inputs.state.is_none() {
        Some("No photograph is open".into())
    } else if declared.is_some_and(|task| task.profile) && profile.is_none() {
        let label = module
            .settings
            .as_ref()
            .and_then(|settings| settings.profiles.as_ref())
            .map_or("profile", |profiles| profiles.label.as_str());
        Some(format!("No {label} profile yet: add one in Settings"))
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
        profiles: if ready.len() > 1 { ready } else { Vec::new() },
        profile,
        state: state_model,
    }
}

/// The profile a task run sends: the one a person chose among several ready ones, else the only
/// ready one, else the first profile so the core can say what it lacks. `None` when the task takes
/// no profile or none exists.
pub(crate) fn task_profile(
    state: &ModuleCapabilities,
    task: &str,
    takes_profile: bool,
) -> Option<String> {
    if !takes_profile {
        return None;
    }
    let profiles = state
        .settings
        .as_ref()
        .map(|settings| settings.profiles.as_slice())
        .unwrap_or_default();
    let ready: Vec<&str> = profiles
        .iter()
        .filter(|profile| profile.status == ProfileStatus::Ready)
        .map(|profile| profile.id.as_str())
        .collect();
    state
        .task_profiles
        .get(task)
        .filter(|chosen| ready.contains(&chosen.as_str()))
        .cloned()
        .or_else(|| ready.first().map(|id| (*id).to_owned()))
        .or_else(|| profiles.first().map(|profile| profile.id.clone()))
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
        title: format!("Allow {} to {verb}?", disclosure.module),
        body: lines.join("\n"),
        actions: vec![
            ("Allow".into(), NoticeAction::AllowConsent),
            ("Don't allow".into(), NoticeAction::DenyConsent),
        ],
    })
}

// ---- evidence -----------------------------------------------------------------------------------

/// What a captured frame's state reports for every capability-declaring module: the settings as the
/// panel shows them (a secret only as `set` or `not set`), profiles, activation, resources, jobs,
/// each task's newest run on the open asset, permissions and the open consent notice. It never
/// holds a secret: none reaches this store except the masked field's text, which is left out.
pub(crate) fn summary(
    store: &CapabilityStore,
    modules: &[ModuleDescriptor],
    open: Option<&EditorState>,
) -> Value {
    Value::Object(
        modules
            .iter()
            .filter(|module| declares(module))
            .map(|module| {
                let empty = ModuleCapabilities::default();
                let state = store.modules.get(&module.id).unwrap_or(&empty);
                (
                    module.id.clone(),
                    module_summary(module, state, store, open),
                )
            })
            .collect(),
    )
}

fn module_summary(
    module: &ModuleDescriptor,
    state: &ModuleCapabilities,
    store: &CapabilityStore,
    open: Option<&EditorState>,
) -> Value {
    let asset = open.map(|open| &open.asset.id);
    let settings = state.settings.as_ref().map(|read| {
        let declared = module.settings.as_ref();
        let fields = |profile: Option<&String>,
                      reads: &BTreeMap<String, FieldRead>,
                      declared: &[SettingDescriptor]| {
            Value::Object(
                declared
                    .iter()
                    .map(|field| {
                        let model = field_model(&module.id, profile, field, reads.get(field.id()), state);
                        (field.id().to_owned(), Value::from(field_text(&model.kind)))
                    })
                    .collect(),
            )
        };
        json!({
            "revision": read.revision,
            "state": read.state,
            "fields": fields(None, &read.fields, declared.map(|settings| settings.fields.as_slice()).unwrap_or_default()),
            "profiles": read.profiles.iter().map(|profile| json!({
                "id": profile.id,
                "adapter": profile.adapter,
                "label": profile.label,
                "status": profile.status,
                "fields": fields(
                    Some(&profile.id),
                    &profile.fields,
                    declared
                        .and_then(|settings| settings.profiles.as_ref())
                        .map(|profiles| profiles.fields.as_slice())
                        .unwrap_or_default(),
                ),
            })).collect::<Vec<_>>(),
        })
    });
    let status = state.status.as_ref();
    let resources: Vec<Value> = module
        .resources
        .iter()
        .map(|declared| {
            let row =
                status.and_then(|status| status.resources.iter().find(|row| row.id == declared.id));
            let model = resource_row(declared, row, state);
            json!({
                "id": declared.id,
                "state": row.map_or(ResourceState::NotInstalled, |row| row.state),
                "progress": model.progress,
                "text": model.state,
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
            let summary = match &run.phase {
                TaskPhase::Requesting => json!({"status": "requesting"}),
                TaskPhase::Consent => json!({"status": "consent"}),
                TaskPhase::NotReady => json!({"status": "not-ready"}),
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
                    "apply": applies.map(|apply| json!({"action": apply.action, "parameter": apply.parameter})),
                    "result": result,
                }),
                TaskPhase::Failed { code, message } => json!({
                    "status": "failed",
                    "error": {"code": code, "message": message},
                }),
            };
            let mut summary = summary;
            summary["asset_id"] = json!(run.asset);
            summary["profile_id"] = json!(run.profile);
            (task.clone(), summary)
        })
        .collect();
    let grants = state.grants();
    let denials = status
        .map(|status| status.permissions.denials.as_slice())
        .unwrap_or_default();
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
        "view": state.view.name(),
        "loaded": state.settings.is_some() || state.status.is_some(),
        "pending": state.pending,
        "settings": settings,
        "activation": status.map(|status| json!({
            "state": status.activation.state,
            "reason": status.activation.reason,
            "text": activation_text(&status.activation, state),
        })),
        "resources": resources,
        "jobs": jobs,
        "tasks": tasks,
        "permissions": {
            "open": state.permissions_open,
            "live": grants.iter().filter(|grant| grant.is_live()).count(),
            "revoked": grants.iter().filter(|grant| !grant.is_live()).count(),
            "denials": denials.len(),
            "grants": grants.iter().map(|grant| json!({
                "grant_id": grant.grant_id,
                "capability": grant.capability,
                "kind": grant.kind,
                "scope": grant.scope,
                "revoked": grant.revoked.as_ref().map(|revoked| &revoked.reason),
            })).collect::<Vec<_>>(),
            "denied": denials.iter().map(|denial| json!({
                "capability": denial.capability,
                "kind": denial.kind,
                "scope": denial.scope,
            })).collect::<Vec<_>>(),
        },
        "requirements": state.requirements,
        "message": state.message,
        "load_error": state.load_error,
        "errors": state.errors.iter().map(|((profile, field), error)| json!({"profile": profile, "field": field, "error": error})).collect::<Vec<_>>(),
        "conflicts": state.conflicts.iter().map(|(profile, field)| json!({"profile": profile, "field": field})).collect::<Vec<_>>(),
        "replacing_secret": state.secret.as_ref().map(|((profile, field), _)| json!({"profile": profile, "field": field})),
        "consent": consent,
    })
}

/// A field exactly as the panel shows it, with a secret reduced to whether it is set.
fn field_text(kind: &FieldKindModel) -> String {
    match kind {
        FieldKindModel::Number { display, .. } | FieldKindModel::Text { display, .. } => {
            display.clone()
        }
        FieldKindModel::Toggle { on } => on.to_string(),
        FieldKindModel::Choice { options, selected } => selected
            .and_then(|index| options.get(index))
            .cloned()
            .unwrap_or_default(),
        FieldKindModel::Secret { state, .. } => state.to_lowercase(),
    }
}
