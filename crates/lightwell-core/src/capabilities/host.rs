//! The capability host the catalog owner holds: where settings, grants and resources live, which
//! secret store holds credentials, the capability worker's lanes and jobs, each module's
//! activation, and the owner-answered methods over all of them. Every call here is a short file,
//! stat or secret-store attribute call; nothing hashes, downloads, loads module state or reads a
//! secret's data on the owner. That work is queued on a lane, whose result comes back into the
//! owner's channel. See `docs/design/module-capabilities.md`.
use super::{
    consent::{consent_required, download_disclosure},
    context::ModuleContext,
    descriptor::{CapabilityDescriptor, CapabilityKind, ResourceDescriptor, SettingKind},
    grants::{
        self, DENY, DownloadScope, GRANT, Grant, GrantKind, GrantScope, GrantsStore, LIST,
        MAX_REASON, NewGrant, REVOKE,
    },
    jobs::{
        Admission, Cancelled, Deliver, JOB_CANCEL, JOB_READ, JobControl, JobError, JobKind,
        JobRecord, JobStatus, Jobs, NewJob, Origin, PERMISSION_REVOKED, Work,
    },
    resources::{
        self, DEFAULT_RESOURCE_QUOTA_BYTES, Fetch, INSTALL, InstallJob, InstallSource, REMOVE,
        RESOURCE_LIST, ResourceRow, ResourceState, ResourceStore, SharedTransport,
    },
    secrets::{SecretStore, SecretValue, UnavailableSecretStore},
    settings::{
        CLEAR_SECRET, CREATE_PROFILE, FieldRead, READ, REMOVE_PROFILE, RESET, SET, SET_SECRET,
        SettingsRead, SettingsState, SettingsStore, SettingsWrite, ValueSource, WriteOutcome,
    },
    transport::{EndpointClass, TransportConfig, parse_endpoint},
};
use crate::{
    ApiRequest, AssetId, Availability, ClientAuthority, EditorService, Error, ErrorKind, JobId,
    ModuleDescriptor, ModuleRegistry, Mutation,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::{
    collections::HashMap,
    panic::{self, AssertUnwindSafe},
    path::{Path, PathBuf},
    sync::Arc,
};

/// The lifecycle methods.
pub const ACTIVATE: &str = "module.activate";
pub const DEACTIVATE: &str = "module.deactivate";
pub const STATUS: &str = "module.status";

/// Why grants are revoked or a module deactivated when something they depend on changes.
pub const ENDPOINT_CHANGED: &str = "endpoint changed";
pub const SELECTION_CHANGED: &str = "selection changed";
pub const PROFILE_REMOVED: &str = "profile removed";
pub const SETTINGS_RESET: &str = "settings reset";
pub const SETTINGS_CHANGED: &str = "settings changed";
pub const RESOURCE_REMOVED: &str = "resource removed";

/// Where the host keeps what it owns for modules. Tests and evidence runs point every directory at
/// an isolated location, pass an in-memory secret store and inject their own transport, so they
/// never touch the person's configuration, login keychain or network.
#[derive(Clone)]
pub struct HostConfig {
    /// The directory that holds `settings.json` and `grants.json`, normally `<config>/modules`.
    /// `None` refuses every settings and permission method with `not-ready`. Nothing is created
    /// until the first write.
    pub config_dir: Option<PathBuf>,
    /// The directory managed resources are installed under, normally `<data>/modules/resources`.
    /// `None` refuses installs and removals with `not-ready`. Nothing is created until an install.
    pub resource_dir: Option<PathBuf>,
    pub secrets: Arc<dyn SecretStore>,
    /// How downloads reach the network: by default the platform's trust store, the system resolver
    /// and direct connections. The transport is built on the first download, never at start.
    pub transport: TransportConfig,
    /// The storage every module's installed resources may take together.
    pub resource_quota_bytes: u64,
}

impl HostConfig {
    /// No directories and no secure store: every settings, permission and resource method reports
    /// `not-ready`.
    pub fn unconfigured() -> Self {
        Self {
            config_dir: None,
            resource_dir: None,
            secrets: Arc::new(UnavailableSecretStore::new("no secure store is configured")),
            transport: TransportConfig::default(),
            resource_quota_bytes: DEFAULT_RESOURCE_QUOTA_BYTES,
        }
    }
}

impl std::fmt::Debug for HostConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostConfig")
            .field("config_dir", &self.config_dir)
            .field("resource_dir", &self.resource_dir)
            .field("secrets", &self.secrets.name())
            .field("trust", &self.transport.trust)
            .field("resource_quota_bytes", &self.resource_quota_bytes)
            .finish()
    }
}

fn validation(detail: impl Into<String>) -> Error {
    Error::new(ErrorKind::Validation, detail)
}

fn params<T: DeserializeOwned>(value: &Value) -> Result<T, Error> {
    serde_json::from_value(value.clone()).map_err(|error| validation(error.to_string()))
}

fn encode(value: impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
}

/// Announce `origin` once, however many changes a request made.
fn announce_once(announce: &mut Vec<Origin>, origin: &Origin) {
    if !announce.contains(origin) {
        announce.push(origin.clone());
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleParams {
    module_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OptionalModuleParams {
    #[serde(default)]
    module_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetParams {
    module_id: String,
    #[serde(default)]
    profile_id: Option<String>,
    values: Map<String, Value>,
    mutation: Mutation,
}

/// `set-secret` without its value, which is taken out of the request before anything else reads
/// it, and `clear-secret`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretParams {
    module_id: String,
    #[serde(default)]
    profile_id: Option<String>,
    setting: String,
    mutation: Mutation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResetParams {
    module_id: String,
    mutation: Mutation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateProfileParams {
    module_id: String,
    adapter: String,
    label: String,
    mutation: Mutation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveProfileParams {
    module_id: String,
    profile_id: String,
    mutation: Mutation,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantParams {
    module_id: String,
    capability: String,
    scope: Value,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DenyParams {
    module_id: String,
    capability: String,
    scope: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeParams {
    grant_id: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobParams {
    job_id: JobId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceParams {
    module_id: String,
    resource_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InstallParams {
    module_id: String,
    resource_id: String,
    #[serde(default)]
    source: InstallSource,
}

/// Split a `set-secret` request into its secret and the rest. The value is moved straight into a
/// [`SecretValue`], and a malformed one is refused without echoing it, which serde's own message
/// for a wrong type would do.
fn secret_params(request: &Value) -> Result<(SecretParams, SecretValue), Error> {
    let mut object = match request {
        Value::Object(object) => object.clone(),
        _ => return Err(validation("params must be a JSON object")),
    };
    let value = match object.remove("value") {
        Some(Value::String(value)) => SecretValue::new(value),
        Some(_) => return Err(validation("value must be a string")),
        None => return Err(validation("missing field `value`")),
    };
    Ok((params(&Value::Object(object))?, value))
}

/// A module's activation state. Nothing activates it but `module.activate`; a new owner starts
/// every module inactive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ActivationState {
    #[default]
    Inactive,
    Activating,
    Active,
    Failed,
}

impl ActivationState {
    pub fn name(self) -> &'static str {
        match self {
            Self::Inactive => "inactive",
            Self::Activating => "activating",
            Self::Active => "active",
            Self::Failed => "failed",
        }
    }
}

/// A module's activation as `module.status` reports it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActivationRead {
    pub state: ActivationState,
    /// Why the module is inactive, when something other than a client deactivated it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The activation job while activating, or the job releasing a deactivated module.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<JobId>,
    /// Why the last activation failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JobError>,
}

/// One unmet activation requirement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// `setting` or `resource`.
    pub kind: String,
    pub id: String,
    /// `missing`, `invalid`, `incompatible` or `unavailable` for a setting; a resource state for a
    /// resource.
    pub state: String,
}

#[derive(Default)]
struct Activation {
    state: ActivationState,
    reason: Option<String>,
    error: Option<JobError>,
    /// The activation job while activating; the release job of a deactivation while it runs.
    job: Option<JobId>,
    /// A deactivation asked for while the activation job ran, with its reason: whatever the job
    /// does, the module ends inactive and anything it loaded is released.
    pending: Option<Option<String>>,
}

impl Activation {
    fn read(&self) -> ActivationRead {
        ActivationRead {
            state: self.state,
            reason: self.reason.clone(),
            job_id: self.job.clone(),
            error: self.error.clone(),
        }
    }

    fn set_inactive(&mut self, reason: Option<String>) {
        self.state = ActivationState::Inactive;
        self.reason = reason;
        self.error = None;
    }
}

/// `{module_id, activation, job_id?, status?}`: what a lifecycle request changed.
fn activation_answer(module_id: &str, state: ActivationState, job: Option<&JobRecord>) -> Value {
    let mut value = json!({"module_id": module_id, "activation": state.name()});
    if let Some(job) = job {
        value["job_id"] = json!(job.job_id);
        value["status"] = json!(job.status);
    }
    value
}

/// The `download-artifact` scope of a declared resource: its identity, version and the origin of
/// its pinned URL.
fn download_scope(resource: &ResourceDescriptor) -> Result<DownloadScope, Error> {
    let endpoint = parse_endpoint(
        &resource.url,
        &[EndpointClass::Remote, EndpointClass::Loopback],
    )?;
    Ok(DownloadScope {
        resource: resource.id.clone(),
        version: resource.version.clone(),
        origin: endpoint.origin(),
    })
}

/// The owner's capability state: the configuration, the stores over its directories, the lanes
/// and their jobs, and each module's activation.
pub(crate) struct CapabilityHost {
    config: HostConfig,
    settings: Option<SettingsStore>,
    grants: Option<GrantsStore>,
    resources: Option<ResourceStore>,
    transport: Arc<SharedTransport>,
    jobs: Jobs,
    activations: HashMap<String, Activation>,
}

impl CapabilityHost {
    /// `deliver` posts a finished job into the owner's channel; it is called on a lane thread.
    pub(crate) fn new(config: HostConfig, deliver: Deliver) -> Self {
        Self {
            settings: config.config_dir.clone().map(SettingsStore::new),
            grants: config.config_dir.clone().map(GrantsStore::new),
            resources: config.resource_dir.clone().map(ResourceStore::new),
            transport: Arc::new(SharedTransport::new(config.transport.clone())),
            jobs: Jobs::new(deliver),
            activations: HashMap::new(),
            config,
        }
    }

    fn not_configured() -> Error {
        Error::new(
            ErrorKind::NotReady,
            "no application data directory is configured",
        )
    }

    fn settings(&self) -> Result<&SettingsStore, Error> {
        self.settings.as_ref().ok_or_else(Self::not_configured)
    }

    fn grants(&self) -> Result<&GrantsStore, Error> {
        self.grants.as_ref().ok_or_else(Self::not_configured)
    }

    fn resources(&self) -> Result<&ResourceStore, Error> {
        self.resources.as_ref().ok_or_else(Self::not_configured)
    }

    fn secrets(&self) -> &dyn SecretStore {
        self.config.secrets.as_ref()
    }

    /// How many capability lane threads have started. Discovery, reads and a reopen start none.
    #[cfg(test)]
    pub(crate) fn lanes_started(&self) -> usize {
        self.jobs.lanes_started()
    }

    /// Answer one capability method, or `None` when the method is not one this host owns.
    /// `authority` is the caller's, for the one method that needs more than editing. Every change a
    /// request makes that other clients should learn of is pushed to `announce`, which the owner
    /// turns into events.
    pub(crate) fn answer(
        &mut self,
        service: &EditorService,
        authority: ClientAuthority,
        request: &ApiRequest,
        announce: &mut Vec<Origin>,
    ) -> Option<Result<Value, Error>> {
        let registry = service.registry();
        let params = &request.params;
        let origin = Origin::new(&request.method, &request.id);
        Some(match request.method.as_str() {
            READ => self.read(registry, params),
            SET | SET_SECRET | CLEAR_SECRET | RESET | CREATE_PROFILE | REMOVE_PROFILE => {
                self.settings_write(registry, request, announce)
            }
            GRANT => self.grant(service, authority, params, &origin, announce),
            DENY => self.deny(registry, authority, params, &origin, announce),
            REVOKE => self.revoke(params, &origin, announce),
            LIST => self.list_permissions(registry, params),
            ACTIVATE => self.activate(registry, params, &origin),
            DEACTIVATE => self.deactivate_request(registry, params, &origin, announce),
            STATUS => self.status(registry, params),
            JOB_READ => self.job_read(params),
            JOB_CANCEL => self.job_cancel(params, &origin, announce),
            RESOURCE_LIST => self.resource_list(registry, params),
            INSTALL => self.install(registry, authority, params, &origin),
            REMOVE => self.remove(registry, params, &origin, announce),
            _ => return None,
        })
    }

    /// A lane finished a job: record it, update the module it belongs to, and announce what
    /// changed under the request that started it. A failed install or removal changes nothing a
    /// client shows, so it announces nothing.
    pub(crate) fn finished(
        &mut self,
        service: &EditorService,
        job_id: &JobId,
        result: Result<Value, Error>,
        announce: &mut Vec<Origin>,
    ) {
        let Some(done) = self.jobs.complete(job_id, result) else {
            return;
        };
        let module_id = done.record.module_id.clone();
        match done.record.kind {
            JobKind::Activate => {
                let changed = self.activation_finished(service.registry(), &done.record);
                if changed && let Some(origin) = &done.origin {
                    announce_once(announce, origin);
                }
            }
            JobKind::Deactivate => {
                if let Some(activation) = self.activations.get_mut(&module_id)
                    && activation.job.as_ref() == Some(&done.record.job_id)
                {
                    activation.job = None;
                }
            }
            JobKind::Install | JobKind::Remove | JobKind::Task => {
                if done.record.status == JobStatus::Succeeded
                    && let Some(origin) = &done.origin
                {
                    announce_once(announce, origin);
                }
            }
        }
    }

    /// Stop the lanes: running jobs are asked to stop and their threads are joined.
    pub(crate) fn shutdown(&mut self) {
        self.jobs.shutdown();
    }

    // Settings.

    fn read(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let request: ModuleParams = params(request)?;
        let descriptor = module(registry, &request.module_id)?;
        encode(self.settings()?.read(descriptor, self.secrets())?)
    }

    /// Every settings write: commit it, then apply what it implies for grants and activation, and
    /// answer with its result and the module's settings as they read now. Neither holds a secret.
    /// A retry answered from the request log implies nothing new: it was applied the first time.
    fn settings_write(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: &ApiRequest,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let (descriptor, write) = self.write_settings(registry, request)?;
        let mut value = encode(&write.result)?;
        if write.result.outcome == WriteOutcome::Committed && !write.result.deduplicated {
            let origin = Origin::new(&request.method, &request.id);
            announce_once(announce, &origin);
            // The write is committed whatever follows, so a grants file that cannot be updated is
            // reported beside the result rather than as the write's failure. Nothing it leaves
            // behind can be used: a grant names its exact path or endpoint origin, which the
            // settings no longer hold.
            match self.after_settings_write(
                registry,
                descriptor,
                &request.method,
                &write,
                &origin,
                announce,
            ) {
                Ok(revoked) if revoked.is_empty() => {}
                Ok(revoked) => {
                    value["revoked"] = json!(
                        revoked
                            .iter()
                            .map(|grant| &grant.grant_id)
                            .collect::<Vec<_>>()
                    );
                }
                Err(error) => {
                    value["revocation_error"] = json!(JobError::from(&error));
                }
            }
        }
        value["settings"] = encode(self.settings()?.read(descriptor, self.secrets())?)?;
        Ok(value)
    }

    fn write_settings<'a>(
        &self,
        registry: &'a ModuleRegistry,
        request: &ApiRequest,
    ) -> Result<(&'a ModuleDescriptor, SettingsWrite), Error> {
        let request_params = &request.params;
        Ok(match request.method.as_str() {
            SET => {
                let request: SetParams = params(request_params)?;
                let descriptor = module(registry, &request.module_id)?;
                let write = self.settings()?.set(
                    descriptor,
                    request.profile_id.as_deref(),
                    &request.values,
                    &request.mutation,
                )?;
                (descriptor, write)
            }
            SET_SECRET => {
                let (request, value) = secret_params(request_params)?;
                let descriptor = module(registry, &request.module_id)?;
                let write = self.settings()?.set_secret(
                    descriptor,
                    self.secrets(),
                    request.profile_id.as_deref(),
                    &request.setting,
                    &value,
                    &request.mutation,
                )?;
                (descriptor, write)
            }
            CLEAR_SECRET => {
                let request: SecretParams = params(request_params)?;
                let descriptor = module(registry, &request.module_id)?;
                let write = self.settings()?.clear_secret(
                    descriptor,
                    self.secrets(),
                    request.profile_id.as_deref(),
                    &request.setting,
                    &request.mutation,
                )?;
                (descriptor, write)
            }
            RESET => {
                let request: ResetParams = params(request_params)?;
                let descriptor = module(registry, &request.module_id)?;
                let write =
                    self.settings()?
                        .reset(descriptor, self.secrets(), &request.mutation)?;
                (descriptor, write)
            }
            CREATE_PROFILE => {
                let request: CreateProfileParams = params(request_params)?;
                let descriptor = module(registry, &request.module_id)?;
                let write = self.settings()?.create_profile(
                    descriptor,
                    &request.adapter,
                    &request.label,
                    &request.mutation,
                )?;
                (descriptor, write)
            }
            REMOVE_PROFILE => {
                let request: RemoveProfileParams = params(request_params)?;
                let descriptor = module(registry, &request.module_id)?;
                let write = self.settings()?.remove_profile(
                    descriptor,
                    self.secrets(),
                    &request.profile_id,
                    &request.mutation,
                )?;
                (descriptor, write)
            }
            method => {
                return Err(Error::new(
                    ErrorKind::Internal,
                    format!("{method} is not a settings write"),
                ));
            }
        })
    }

    /// What a committed settings write implies. A changed field declared `invalidates_activation`
    /// deactivates an active or activating module. Grants scoped to a value the write replaced or
    /// removed are revoked, and the jobs running under them are cancelled: a profile's remote
    /// grants when its endpoint changes or it is removed, a file setting's read grants for the path
    /// it held, and a reset's file and remote grants. Download grants name a pinned resource, not a
    /// setting, and survive a reset.
    fn after_settings_write(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        descriptor: &ModuleDescriptor,
        method: &str,
        write: &SettingsWrite,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Vec<Grant>, Error> {
        let module_id = descriptor.id.as_str();
        let result = &write.result;
        if result.invalidates_activation {
            self.deactivate(
                registry,
                module_id,
                Some(SETTINGS_CHANGED.to_owned()),
                Some(origin),
                announce,
            )?;
        }
        let Some(grants) = &self.grants else {
            return Ok(Vec::new());
        };
        let Some(settings) = &descriptor.settings else {
            return Ok(Vec::new());
        };
        let remote_of = |profile: &str, grant: &Grant| {
            grant.module_id == module_id
                && matches!(&grant.scope, GrantScope::Remote(scope) if scope.profile_id == profile)
        };
        let mut revoked = Vec::new();
        match method {
            SET => match &result.profile_id {
                Some(profile) => {
                    let endpoint = result.changed.iter().any(|id| {
                        settings
                            .profiles
                            .as_ref()
                            .and_then(|profiles| profiles.field(id))
                            .is_some_and(|field| matches!(field.kind, SettingKind::Endpoint { .. }))
                    });
                    if endpoint {
                        revoked.extend(grants.revoke_matching(
                            |grant| remote_of(profile, grant),
                            ENDPOINT_CHANGED,
                        )?);
                    }
                }
                None => {
                    for id in &result.changed {
                        let is_file = settings
                            .field(id)
                            .is_some_and(|field| matches!(field.kind, SettingKind::File { .. }));
                        let Some(Value::String(previous)) = write.previous.get(id) else {
                            continue;
                        };
                        if !is_file {
                            continue;
                        }
                        let readers: Vec<&str> = descriptor
                            .capabilities
                            .iter()
                            .filter(|capability| {
                                matches!(&capability.kind, CapabilityKind::ReadUserFile { setting } if setting == id)
                            })
                            .map(|capability| capability.id.as_str())
                            .collect();
                        revoked.extend(grants.revoke_matching(
                            |grant| {
                                grant.module_id == module_id
                                    && readers.contains(&grant.capability.as_str())
                                    && matches!(&grant.scope, GrantScope::File(scope) if scope.path == Path::new(previous))
                            },
                            SELECTION_CHANGED,
                        )?);
                    }
                }
            },
            REMOVE_PROFILE => {
                for profile in &write.removed {
                    revoked.extend(
                        grants.revoke_matching(
                            |grant| remote_of(&profile.id, grant),
                            PROFILE_REMOVED,
                        )?,
                    );
                }
            }
            RESET => {
                revoked.extend(grants.revoke_matching(
                    |grant| {
                        grant.module_id == module_id
                            && matches!(
                                grant.kind,
                                GrantKind::ReadUserFile | GrantKind::RemoteImageRequest
                            )
                    },
                    SETTINGS_RESET,
                )?);
            }
            _ => {}
        }
        for grant in &revoked {
            self.cancel_dependents_of(&grant.grant_id, origin, announce);
        }
        Ok(revoked)
    }

    // Permissions.

    /// `module.permission.grant`: only a client with permission authority, only for a scope the
    /// module can use now. A retry of the same `request_id` returns the grant it made.
    fn grant(
        &mut self,
        service: &EditorService,
        authority: ClientAuthority,
        request: &Value,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        if authority != ClientAuthority::Permissions {
            return Err(Error::new(
                ErrorKind::Forbidden,
                "granting a permission needs permission authority",
            ));
        }
        let request: GrantParams = params(request)?;
        if request.request_id.is_empty() || request.request_id.len() > 128 {
            return Err(validation("request_id must contain 1..128 characters"));
        }
        let registry = service.registry();
        let descriptor = registered(registry, &request.module_id)?;
        let capability = declared_capability(descriptor, &request.capability)?;
        let scope = GrantScope::parse(GrantKind::of(&capability.kind), &request.scope)?;
        let grants = self.grants()?;
        // A retry is recognised before the scope is checked again, so it returns what the first
        // request did even if the scope has since stopped being usable.
        if let Some(previous) = grants.request(&request.module_id, &request.request_id)? {
            if previous.capability != capability.id || previous.scope != scope {
                return Err(Error::new(
                    ErrorKind::Conflict,
                    "request_id was already used with different input",
                ));
            }
            return encode(grants::GrantOutcome {
                grant: previous,
                outcome: WriteOutcome::NoOp,
                deduplicated: true,
            });
        }
        self.check_usable(service, descriptor, capability, &scope)?;
        let outcome = self.grants()?.grant(NewGrant {
            module_id: &descriptor.id,
            capability: &capability.id,
            scope,
            actor: authority.label(),
            request_id: &request.request_id,
        })?;
        if outcome.outcome == WriteOutcome::Committed && !outcome.deduplicated {
            announce_once(announce, origin);
        }
        encode(outcome)
    }

    /// Whether the module can use `scope` now: the file its setting holds, the resource version it
    /// declares from its pinned origin, or a profile of the capability's adapter whose endpoint has
    /// that origin, the capability's data class and an asset of this catalog.
    fn check_usable(
        &self,
        service: &EditorService,
        descriptor: &ModuleDescriptor,
        capability: &CapabilityDescriptor,
        scope: &GrantScope,
    ) -> Result<(), Error> {
        match (&capability.kind, scope) {
            (CapabilityKind::ReadUserFile { setting }, GrantScope::File(scope)) => {
                let read = self.settings()?.read(descriptor, self.secrets())?;
                let holds = matches!(
                    read.fields.get(setting),
                    Some(FieldRead::Value {
                        value: Value::String(path),
                        source: ValueSource::User,
                        valid: true,
                        ..
                    }) if Path::new(path) == scope.path
                );
                if !holds {
                    return Err(validation(format!(
                        "{} is not the file setting {setting} holds now",
                        scope.path.display()
                    )));
                }
            }
            (CapabilityKind::DownloadArtifact { resource }, GrantScope::Download(scope)) => {
                let declared = descriptor.resource(resource).ok_or_else(|| {
                    Error::new(
                        ErrorKind::Internal,
                        format!("capability {} names an undeclared resource", capability.id),
                    )
                })?;
                if *scope != download_scope(declared)? {
                    return Err(validation(format!(
                        "the scope must name resource {resource} version {} from the origin of its pinned URL",
                        declared.version
                    )));
                }
            }
            (CapabilityKind::RemoteImageRequest { adapter, data }, GrantScope::Remote(scope)) => {
                if scope.adapter != *adapter || scope.data != *data {
                    return Err(validation(format!(
                        "capability {} sends {} through adapter {adapter}",
                        capability.id,
                        data.name()
                    )));
                }
                let read = self.settings()?.read(descriptor, self.secrets())?;
                let profile = read
                    .profile(&scope.profile_id)
                    .ok_or_else(|| validation(format!("unknown profile {}", scope.profile_id)))?;
                if profile.adapter != *adapter {
                    return Err(validation(format!(
                        "profile {} uses adapter {}, not {adapter}",
                        profile.id, profile.adapter
                    )));
                }
                let origin = profile_origin(descriptor, &profile.fields).ok_or_else(|| {
                    validation(format!("profile {} has no valid endpoint", profile.id))
                })?;
                if origin != scope.origin {
                    return Err(validation(format!(
                        "profile {} sends to {origin}, not {}",
                        profile.id, scope.origin
                    )));
                }
                asset_exists(service, &scope.asset_id)?;
            }
            _ => {
                return Err(Error::new(
                    ErrorKind::Internal,
                    "a scope was parsed as another capability kind",
                ));
            }
        }
        Ok(())
    }

    /// `module.permission.deny`: record a "Don't allow" for one exact scope. Any client may.
    fn deny(
        &mut self,
        registry: &ModuleRegistry,
        authority: ClientAuthority,
        request: &Value,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let request: DenyParams = params(request)?;
        let descriptor = registered(registry, &request.module_id)?;
        let capability = declared_capability(descriptor, &request.capability)?;
        let scope = GrantScope::parse(GrantKind::of(&capability.kind), &request.scope)?;
        let denial =
            self.grants()?
                .deny(&descriptor.id, &capability.id, scope, authority.label())?;
        announce_once(announce, origin);
        Ok(json!({"denial": denial}))
    }

    /// `module.permission.revoke`: mark the grant revoked and cancel the jobs running under it.
    /// Any client may. Recipes, history and accepted artifacts are never touched.
    fn revoke(
        &mut self,
        request: &Value,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let request: RevokeParams = params(request)?;
        let reason = request.reason.unwrap_or_else(|| "revoked".to_owned());
        if reason.trim().is_empty() || reason.chars().count() > MAX_REASON {
            return Err(validation(format!(
                "a revocation reason is 1..={MAX_REASON} characters"
            )));
        }
        let (grant, changed) = self.grants()?.revoke(&request.grant_id, &reason)?;
        let cancelled = if changed {
            announce_once(announce, origin);
            self.cancel_dependents_of(&grant.grant_id, origin, announce)
        } else {
            Vec::new()
        };
        Ok(json!({
            "outcome": if changed { WriteOutcome::Committed } else { WriteOutcome::NoOp },
            "grant": grant,
            "cancelled_jobs": cancelled,
        }))
    }

    /// `module.permission.list`: grants, revoked ones included, and denials.
    fn list_permissions(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let request: OptionalModuleParams = params(request)?;
        if let Some(module_id) = &request.module_id {
            registered(registry, module_id)?;
        }
        encode(self.grants()?.list(request.module_id.as_deref())?)
    }

    /// Cancel the live jobs running under this grant, as `permission revoked`.
    fn cancel_dependents_of(
        &mut self,
        grant_id: &str,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Vec<JobId> {
        let dependents = self.jobs.depending_on(grant_id);
        for job_id in &dependents {
            self.cancel_job(job_id, PERMISSION_REVOKED, origin, announce);
        }
        dependents
    }

    // Jobs.

    fn job_read(&self, request: &Value) -> Result<Value, Error> {
        let request: JobParams = params(request)?;
        encode(
            self.jobs
                .read(&request.job_id)
                .ok_or_else(|| unknown_job(&request.job_id))?,
        )
    }

    /// `module.job.cancel`: any client may, since a capability job belongs to its module. A
    /// deactivation releases what a module holds and is never cancelled.
    fn job_cancel(
        &mut self,
        request: &Value,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let request: JobParams = params(request)?;
        let record = self
            .jobs
            .read(&request.job_id)
            .ok_or_else(|| unknown_job(&request.job_id))?;
        if record.kind == JobKind::Deactivate && !record.status.is_finished() {
            return Err(Error::new(
                ErrorKind::Conflict,
                "a deactivation releases what the module holds and cannot be cancelled",
            ));
        }
        let record = self
            .cancel_job(&request.job_id, "the job was cancelled", origin, announce)
            .unwrap_or(record);
        encode(record)
    }

    /// Cancel one job and apply what that means for its module: a waiting activation leaves the
    /// module inactive at once; a running one ends inactive when it stops, and anything it loaded
    /// is released. A waiting job's removal is announced; a running one's stop is announced when
    /// it finishes.
    fn cancel_job(
        &mut self,
        job_id: &JobId,
        reason: &str,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Option<JobRecord> {
        match self.jobs.cancel(job_id, reason)? {
            Cancelled::Removed(record) => {
                if record.kind == JobKind::Activate
                    && let Some(activation) = self.activations.get_mut(&record.module_id)
                    && activation.job.as_ref() == Some(&record.job_id)
                {
                    activation.job = None;
                    activation.set_inactive(None);
                }
                announce_once(announce, origin);
                Some(record)
            }
            Cancelled::Requested(record) => {
                if record.kind == JobKind::Activate
                    && let Some(activation) = self.activations.get_mut(&record.module_id)
                    && activation.job.as_ref() == Some(&record.job_id)
                    && activation.pending.is_none()
                {
                    activation.pending = Some(None);
                }
                Some(record)
            }
            Cancelled::Finished(record) => Some(record),
        }
    }

    // Activation.

    /// `module.activate`: check every declared requirement and queue the activation on the module
    /// lane, or join the one already queued or running. An active module answers at once.
    fn activate(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: &Value,
        origin: &Origin,
    ) -> Result<Value, Error> {
        let request: ModuleParams = params(request)?;
        let descriptor = registered(registry, &request.module_id)?;
        if let Availability::Unavailable { reason } = &descriptor.availability {
            return Err(validation(format!(
                "module {} is unavailable: {reason}",
                descriptor.id
            )));
        }
        let declared = descriptor.activation.as_ref().ok_or_else(|| {
            validation(format!("module {} declares no activation", descriptor.id))
        })?;
        let module_id = descriptor.id.as_str();
        if let Some(activation) = self.activations.get(module_id) {
            match activation.state {
                ActivationState::Active => {
                    return Ok(activation_answer(module_id, ActivationState::Active, None));
                }
                ActivationState::Activating if activation.pending.is_none() => {
                    let job = activation.job.as_ref().and_then(|job| self.jobs.read(job));
                    return Ok(activation_answer(
                        module_id,
                        ActivationState::Activating,
                        job.as_ref(),
                    ));
                }
                // The running activation was asked to stop, so a new one is queued behind it; the
                // lane runs them in order, and the stopped one's result is no longer the module's.
                ActivationState::Activating
                | ActivationState::Inactive
                | ActivationState::Failed => {}
            }
        }
        let settings = match &descriptor.settings {
            Some(_) => match self
                .settings()
                .and_then(|store| store.read(descriptor, self.secrets()))
            {
                Ok(read) => Some(read),
                Err(error) if !declared.requires_settings.is_empty() => return Err(error),
                Err(_) => None,
            },
            None => None,
        };
        let mut missing = Vec::new();
        for id in &declared.requires_settings {
            if let Some(state) = setting_requirement(settings.as_ref(), id) {
                missing.push(Requirement {
                    kind: "setting".into(),
                    id: id.clone(),
                    state: state.into(),
                });
            }
        }
        let rows = self.resource_rows(descriptor);
        for id in &declared.requires_resources {
            let state = rows
                .iter()
                .find(|row| &row.id == id)
                .map_or(ResourceState::NotInstalled, |row| row.state);
            if state != ResourceState::Installed {
                missing.push(Requirement {
                    kind: "resource".into(),
                    id: id.clone(),
                    state: state.name().to_owned(),
                });
            }
        }
        if !missing.is_empty() {
            let list = missing
                .iter()
                .map(|requirement| {
                    format!(
                        "{} {} is {}",
                        requirement.kind, requirement.id, requirement.state
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            return Err(Error::new(
                ErrorKind::NotReady,
                format!("module {module_id} is not ready to activate: {list}"),
            )
            .with_data(json!({"requirements": missing})));
        }
        let control = JobControl::new();
        let mut context =
            ModuleContext::new(module_id, self.config.secrets.clone(), control.clone())
                .with_settings(
                    effective_values(settings.as_ref()),
                    secret_fields(descriptor),
                );
        for row in &rows {
            if let (ResourceState::Installed, Some(path)) = (row.state, &row.path) {
                context = context.with_resource(&row.id, path.clone());
            }
        }
        let work: Work = {
            let registry = registry.clone();
            let module_id = module_id.to_owned();
            let control = control.clone();
            Box::new(move || {
                let module = registry.module(&module_id).ok_or_else(|| {
                    Error::new(
                        ErrorKind::Internal,
                        format!("module {module_id} is not registered"),
                    )
                })?;
                let outcome = panic::catch_unwind(AssertUnwindSafe(|| module.activate(&context)))
                    .unwrap_or_else(|_| {
                        Err(Error::new(
                            ErrorKind::Internal,
                            format!("the activation of module {module_id} stopped unexpectedly"),
                        ))
                    });
                // An activation asked to stop as it finished counts as cancelled.
                let outcome = match outcome {
                    Ok(()) if control.is_cancelled() => Err(control.cancelled_error()),
                    other => other,
                };
                if outcome.is_err() {
                    module.deactivate();
                }
                outcome.map(|()| json!({"activation": ActivationState::Active.name()}))
            })
        };
        let job = self.jobs.submit(
            NewJob {
                job_id: JobId::new(),
                kind: JobKind::Activate,
                module_id: module_id.to_owned(),
                resource_id: None,
                origin: Some(origin.clone()),
                grants: Vec::new(),
                admission: Admission::Bounded,
            },
            control,
            work,
        )?;
        let activation = self.activations.entry(module_id.to_owned()).or_default();
        activation.state = ActivationState::Activating;
        activation.reason = None;
        activation.error = None;
        activation.pending = None;
        activation.job = Some(job.job_id.clone());
        Ok(activation_answer(
            module_id,
            ActivationState::Activating,
            Some(&job),
        ))
    }

    /// `module.deactivate`: a client's explicit deactivation, which records no reason.
    fn deactivate_request(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: &Value,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let request: ModuleParams = params(request)?;
        let descriptor = registered(registry, &request.module_id)?;
        let job = self.deactivate(registry, &descriptor.id, None, Some(origin), announce)?;
        let state = self
            .activations
            .get(&descriptor.id)
            .map_or(ActivationState::Inactive, |activation| activation.state);
        Ok(activation_answer(&descriptor.id, state, job.as_ref()))
    }

    /// Deactivate a module: a waiting activation is superseded and the module reads inactive; a
    /// running one is cancelled and the module ends inactive when it stops; an active module reads
    /// inactive at once and a release job calls its `deactivate` on the module lane, after the work
    /// queued before it. Nothing is deleted. Returns the job the change concerns.
    fn deactivate(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        module_id: &str,
        reason: Option<String>,
        origin: Option<&Origin>,
        announce: &mut Vec<Origin>,
    ) -> Result<Option<JobRecord>, Error> {
        let Some(activation) = self.activations.get(module_id) else {
            return Ok(None);
        };
        let announced = |announce: &mut Vec<Origin>| {
            if let Some(origin) = origin {
                announce_once(announce, origin);
            }
        };
        match activation.state {
            ActivationState::Inactive => Ok(None),
            ActivationState::Failed => {
                self.activation(module_id).set_inactive(reason);
                announced(announce);
                Ok(None)
            }
            ActivationState::Activating => {
                let job_id = activation
                    .job
                    .clone()
                    .expect("an activating module has its job");
                if let Some(record) = self.jobs.supersede(&job_id) {
                    let activation = self.activation(module_id);
                    activation.job = None;
                    activation.set_inactive(reason);
                    announced(announce);
                    return Ok(Some(record));
                }
                let cancel = reason.as_deref().unwrap_or("the module was deactivated");
                let record = match self.jobs.cancel(&job_id, cancel) {
                    Some(Cancelled::Requested(record) | Cancelled::Finished(record)) => record,
                    Some(Cancelled::Removed(record)) => record,
                    None => return Ok(None),
                };
                self.activation(module_id).pending = Some(reason);
                Ok(Some(record))
            }
            ActivationState::Active => {
                let record = self.release(registry, module_id, reason, origin.cloned())?;
                announced(announce);
                Ok(Some(record))
            }
        }
    }

    fn activation(&mut self, module_id: &str) -> &mut Activation {
        self.activations.entry(module_id.to_owned()).or_default()
    }

    /// Queue the release of what an active module loaded, which is always admitted, and mark the
    /// module inactive.
    fn release(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        module_id: &str,
        reason: Option<String>,
        origin: Option<Origin>,
    ) -> Result<JobRecord, Error> {
        let registry = registry.clone();
        let owned = module_id.to_owned();
        let work: Work = Box::new(move || {
            if let Some(module) = registry.module(&owned) {
                module.deactivate();
            }
            Ok(json!({"activation": ActivationState::Inactive.name()}))
        });
        let record = self.jobs.submit(
            NewJob {
                job_id: JobId::new(),
                kind: JobKind::Deactivate,
                module_id: module_id.to_owned(),
                resource_id: None,
                origin,
                grants: Vec::new(),
                admission: Admission::Always,
            },
            JobControl::new(),
            work,
        )?;
        let activation = self.activation(module_id);
        activation.set_inactive(reason);
        activation.pending = None;
        activation.job = Some(record.job_id.clone());
        Ok(record)
    }

    /// An activation job finished. Returns whether the module's state changed.
    fn activation_finished(&mut self, registry: &Arc<ModuleRegistry>, record: &JobRecord) -> bool {
        let Some(activation) = self.activations.get_mut(&record.module_id) else {
            return false;
        };
        if activation.job.as_ref() != Some(&record.job_id) {
            return false;
        }
        activation.job = None;
        let pending = activation.pending.take();
        match (record.status, pending) {
            (JobStatus::Succeeded, None) => {
                activation.state = ActivationState::Active;
                activation.reason = None;
                activation.error = None;
            }
            // It finished loading just as it was asked to stop: release what it loaded.
            (JobStatus::Succeeded, Some(reason)) => {
                let origin = None;
                if self
                    .release(registry, &record.module_id, reason.clone(), origin)
                    .is_err()
                {
                    self.activation(&record.module_id).set_inactive(reason);
                }
            }
            (JobStatus::Failed, None) => {
                activation.state = ActivationState::Failed;
                activation.reason = None;
                activation.error = record.error.clone();
            }
            (_, pending) => activation.set_inactive(pending.flatten()),
        }
        true
    }

    // Status.

    /// `module.status`: activation, settings validity, resources, grants and this module's jobs.
    /// A settings read, stats of installed markers and a grants file read; no hashing.
    fn status(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let request: ModuleParams = params(request)?;
        let descriptor = registered(registry, &request.module_id)?;
        let activation = self
            .activations
            .get(&descriptor.id)
            .map_or_else(|| Activation::default().read(), Activation::read);
        let settings = match &descriptor.settings {
            None => Value::Null,
            Some(_) => match self
                .settings()
                .and_then(|store| store.read(descriptor, self.secrets()))
            {
                Ok(read) => {
                    let missing: Vec<&String> = read
                        .fields
                        .iter()
                        .filter(|(_, field)| !field.valid())
                        .map(|(id, _)| id)
                        .collect();
                    json!({"state": read.state, "revision": read.revision, "missing": missing})
                }
                Err(error) => json!({"state": "unavailable", "error": JobError::from(&error)}),
            },
        };
        let permissions = match self
            .grants()
            .and_then(|grants| grants.list(Some(&descriptor.id)))
        {
            Ok(list) => encode(list)?,
            Err(error) => json!({"grants": [], "denials": [], "error": JobError::from(&error)}),
        };
        Ok(json!({
            "module_id": descriptor.id,
            "activation": activation,
            "settings": settings,
            "resources": self.resource_rows(descriptor),
            "permissions": permissions,
            "jobs": self.jobs.of_module(&descriptor.id),
        }))
    }

    // Resources.

    /// Every declared resource of a module with its state: installed by its marker, installing or
    /// failed by its jobs, otherwise not installed.
    fn resource_rows(&self, descriptor: &ModuleDescriptor) -> Vec<ResourceRow> {
        descriptor
            .resources
            .iter()
            .map(|resource| {
                let mut row = ResourceRow {
                    id: resource.id.clone(),
                    title: resource.title.clone(),
                    version: resource.version.clone(),
                    bytes: resource.bytes,
                    sha256: resource.sha256.clone(),
                    license: resource.license.clone(),
                    provenance: resource.provenance.clone(),
                    url: resource.url.clone(),
                    state: ResourceState::NotInstalled,
                    path: None,
                    installed_ms: None,
                    job_id: None,
                    error: None,
                };
                let installed = self.resources.as_ref().and_then(|store| {
                    store
                        .installed(&descriptor.id, resource)
                        .map(|marker| (store, marker))
                });
                if let Some((store, marker)) = installed {
                    row.state = ResourceState::Installed;
                    row.path = Some(store.file_path(&descriptor.id, resource));
                    row.installed_ms = Some(marker.installed_ms);
                } else if let Some(job) =
                    self.jobs
                        .live(JobKind::Install, &descriptor.id, Some(&resource.id))
                {
                    row.state = ResourceState::Installing;
                    row.job_id = Some(job.job_id);
                } else if let Some(job) = self
                    .jobs
                    .last_finished(JobKind::Install, &descriptor.id, Some(&resource.id))
                    .filter(|job| job.status == JobStatus::Failed)
                {
                    row.state = ResourceState::Failed;
                    row.job_id = Some(job.job_id);
                    row.error = job.error;
                }
                row
            })
            .collect()
    }

    /// `module.resource.list`: the declared resources and the storage they share.
    fn resource_list(&self, registry: &ModuleRegistry, request: &Value) -> Result<Value, Error> {
        let request: ModuleParams = params(request)?;
        let descriptor = registered(registry, &request.module_id)?;
        let storage = self.resources.as_ref().map(|store| {
            json!({
                "root": store.root(),
                "used_bytes": store.used_bytes(),
                "quota_bytes": self.config.resource_quota_bytes,
            })
        });
        Ok(json!({
            "module_id": descriptor.id,
            "resources": self.resource_rows(descriptor),
            "storage": storage,
        }))
    }

    /// `module.resource.install`: from the pinned URL under a `download-artifact` grant, or from a
    /// local file whose bytes must match the pinned hash. Consent, the quota and the lane bound
    /// are checked before anything is queued; a second request joins the queued or running
    /// install, and an installed resource answers at once.
    fn install(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        authority: ClientAuthority,
        request: &Value,
        origin: &Origin,
    ) -> Result<Value, Error> {
        let request: InstallParams = params(request)?;
        let descriptor = registered(registry, &request.module_id)?;
        let resource = declared_resource(descriptor, &request.resource_id)?;
        let store = self.resources()?;
        let answer = |state: ResourceState, job: Option<&JobRecord>| {
            let mut value = json!({
                "module_id": descriptor.id,
                "resource_id": resource.id,
                "state": state,
            });
            if let Some(job) = job {
                value["job_id"] = json!(job.job_id);
                value["status"] = json!(job.status);
            }
            value
        };
        if store.installed(&descriptor.id, resource).is_some() {
            return Ok(answer(ResourceState::Installed, None));
        }
        if let Some(job) = self
            .jobs
            .live(JobKind::Install, &descriptor.id, Some(&resource.id))
        {
            return Ok(answer(ResourceState::Installing, Some(&job)));
        }
        let (fetch, grants) = match request.source {
            InstallSource::Download => {
                let capability = descriptor
                    .capabilities
                    .iter()
                    .find(|capability| {
                        matches!(&capability.kind, CapabilityKind::DownloadArtifact { resource: id } if *id == resource.id)
                    })
                    .ok_or_else(|| {
                        validation(format!(
                            "module {} declares no download-artifact capability for resource {}",
                            descriptor.id, resource.id
                        ))
                    })?;
                let scope = GrantScope::Download(download_scope(resource)?);
                let (grant, denied) =
                    self.grants()?
                        .consent(&descriptor.id, &capability.id, &scope)?;
                let Some(grant) = grant else {
                    let install_dir = store.version_dir(&descriptor.id, resource);
                    return Err(consent_required(
                        descriptor,
                        capability,
                        &scope,
                        download_disclosure(descriptor, capability, resource, &install_dir),
                        denied,
                    ));
                };
                (
                    Fetch::Download(self.transport.clone()),
                    vec![grant.grant_id],
                )
            }
            InstallSource::File { path } => {
                let is_file = std::fs::metadata(&path).is_ok_and(|metadata| metadata.is_file());
                if !is_file {
                    return Err(validation(format!(
                        "{} is not a readable file",
                        path.display()
                    )));
                }
                (Fetch::File(path), Vec::new())
            }
        };
        resources::check_quota(store, resource, self.config.resource_quota_bytes)?;
        let job_id = JobId::new();
        let control = JobControl::new();
        let job = InstallJob {
            store: store.clone(),
            job_id: job_id.clone(),
            module_id: descriptor.id.clone(),
            resource: resource.clone(),
            fetch,
            registry: registry.clone(),
            quota: self.config.resource_quota_bytes,
            actor: authority.label().to_owned(),
            control: control.clone(),
        };
        let record = self.jobs.submit(
            NewJob {
                job_id,
                kind: JobKind::Install,
                module_id: descriptor.id.clone(),
                resource_id: Some(resource.id.clone()),
                origin: Some(origin.clone()),
                grants: grants.clone(),
                admission: Admission::Bounded,
            },
            control,
            Box::new(move || resources::install(job)),
        )?;
        // When a grant was last used is a record for the person, not a condition of the job, so a
        // grants file that cannot be written does not refuse an admitted install.
        if let Some(store) = &self.grants {
            let _ = store.touch(&grants);
        }
        Ok(answer(ResourceState::Installing, Some(&record)))
    }

    /// `module.resource.remove`: queue the removal of the installed version, joining one already
    /// queued. A module that is active or activating and requires the resource is deactivated first.
    fn remove(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: &Value,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let request: ResourceParams = params(request)?;
        let descriptor = registered(registry, &request.module_id)?;
        let resource = declared_resource(descriptor, &request.resource_id)?;
        let store = self.resources()?.clone();
        let answer = |job: Option<&JobRecord>, state: ResourceState| {
            let mut value = json!({
                "module_id": descriptor.id,
                "resource_id": resource.id,
                "state": state,
            });
            if let Some(job) = job {
                value["job_id"] = json!(job.job_id);
                value["status"] = json!(job.status);
            }
            value
        };
        let state = self
            .resource_rows(descriptor)
            .into_iter()
            .find(|row| row.id == resource.id)
            .map_or(ResourceState::NotInstalled, |row| row.state);
        if let Some(job) = self
            .jobs
            .live(JobKind::Remove, &descriptor.id, Some(&resource.id))
        {
            return Ok(answer(Some(&job), state));
        }
        if !store.version_dir(&descriptor.id, resource).exists()
            && state != ResourceState::Installing
        {
            return Ok(answer(None, ResourceState::NotInstalled));
        }
        let control = JobControl::new();
        let work: Work = {
            let control = control.clone();
            let module_id = descriptor.id.clone();
            let resource = resource.clone();
            Box::new(move || resources::remove(&store, &module_id, &resource, &control))
        };
        let record = self.jobs.submit(
            NewJob {
                job_id: JobId::new(),
                kind: JobKind::Remove,
                module_id: descriptor.id.clone(),
                resource_id: Some(resource.id.clone()),
                origin: Some(origin.clone()),
                grants: Vec::new(),
                admission: Admission::Bounded,
            },
            control,
            work,
        )?;
        let required = descriptor
            .activation
            .as_ref()
            .is_some_and(|activation| activation.requires_resources.contains(&resource.id));
        if required {
            self.deactivate(
                registry,
                &descriptor.id,
                Some(RESOURCE_REMOVED.to_owned()),
                Some(origin),
                announce,
            )?;
        }
        Ok(answer(Some(&record), state))
    }
}

/// Which requirement a required setting fails, if any: `missing`, `invalid`, `incompatible` or
/// `unavailable` (the secret store could not say).
fn setting_requirement(read: Option<&SettingsRead>, id: &str) -> Option<&'static str> {
    let Some(read) = read else {
        return Some("unavailable");
    };
    if read.state == SettingsState::Incompatible {
        return Some("incompatible");
    }
    match read.fields.get(id) {
        None => Some("missing"),
        Some(FieldRead::Value {
            value: Value::Null, ..
        }) => Some("missing"),
        Some(FieldRead::Value { valid: false, .. }) => Some("invalid"),
        Some(FieldRead::Secret {
            secret_present: Some(false),
            ..
        }) => Some("missing"),
        Some(FieldRead::Secret {
            secret_present: None,
            ..
        }) => Some("unavailable"),
        Some(_) => None,
    }
}

/// The valid, non-null, non-secret module-level values of a settings read.
fn effective_values(read: Option<&SettingsRead>) -> Map<String, Value> {
    read.map(|read| {
        read.fields
            .iter()
            .filter_map(|(id, field)| match field {
                FieldRead::Value {
                    value, valid: true, ..
                } if !value.is_null() => Some((id.clone(), value.clone())),
                _ => None,
            })
            .collect()
    })
    .unwrap_or_default()
}

/// The module-level secret fields a module declares.
fn secret_fields(descriptor: &ModuleDescriptor) -> Vec<String> {
    descriptor
        .settings
        .iter()
        .flat_map(|settings| settings.fields.iter())
        .filter(|field| matches!(field.kind, SettingKind::Secret { .. }))
        .map(|field| field.id.clone())
        .collect()
}

/// The origin a profile's first endpoint field sends to, when it holds a valid value.
fn profile_origin(
    descriptor: &ModuleDescriptor,
    fields: &std::collections::BTreeMap<String, FieldRead>,
) -> Option<String> {
    let profiles = descriptor.settings.as_ref()?.profiles.as_ref()?;
    let (field, classes) = profiles.fields.iter().find_map(|field| match &field.kind {
        SettingKind::Endpoint { classes } => Some((field, classes)),
        _ => None,
    })?;
    match fields.get(&field.id)? {
        FieldRead::Value {
            value: Value::String(url),
            valid: true,
            ..
        } => parse_endpoint(url, classes)
            .ok()
            .map(|endpoint| endpoint.origin()),
        _ => None,
    }
}

fn asset_exists(service: &EditorService, asset_id: &AssetId) -> Result<(), Error> {
    service
        .state(asset_id)
        .map(|_| ())
        .map_err(|_| validation(format!("asset {asset_id} is not in this catalog")))
}

fn unknown_job(job_id: &JobId) -> Error {
    validation(format!("unknown capability job {job_id}"))
}

/// Any registered module.
fn registered<'a>(registry: &'a ModuleRegistry, id: &str) -> Result<&'a ModuleDescriptor, Error> {
    registry
        .module(id)
        .map(|module| module.descriptor())
        .ok_or_else(|| validation(format!("unknown module {id}")))
}

/// The registered module a settings method names, which must declare settings.
fn module<'a>(registry: &'a ModuleRegistry, id: &str) -> Result<&'a ModuleDescriptor, Error> {
    let descriptor = registered(registry, id)?;
    if descriptor.settings.is_none() {
        return Err(validation(format!("module {id} declares no settings")));
    }
    Ok(descriptor)
}

fn declared_capability<'a>(
    descriptor: &'a ModuleDescriptor,
    id: &str,
) -> Result<&'a CapabilityDescriptor, Error> {
    descriptor.capability(id).ok_or_else(|| {
        validation(format!(
            "module {} declares no capability {id}",
            descriptor.id
        ))
    })
}

fn declared_resource<'a>(
    descriptor: &'a ModuleDescriptor,
    id: &str,
) -> Result<&'a ResourceDescriptor, Error> {
    descriptor.resource(id).ok_or_else(|| {
        validation(format!(
            "module {} declares no resource {id}",
            descriptor.id
        ))
    })
}

/// Every owner-answered capability method in the order the method table lists them, and whether
/// it writes, for the method table's tests.
#[cfg(test)]
pub(crate) const METHODS: &[(&str, bool)] = &[
    (READ, false),
    (SET, true),
    (SET_SECRET, true),
    (CLEAR_SECRET, true),
    (RESET, true),
    (CREATE_PROFILE, true),
    (REMOVE_PROFILE, true),
    (GRANT, true),
    (DENY, true),
    (REVOKE, true),
    (LIST, false),
    (ACTIVATE, true),
    (DEACTIVATE, true),
    (STATUS, false),
    (RESOURCE_LIST, false),
    (INSTALL, true),
    (REMOVE, true),
    (JOB_READ, false),
    (JOB_CANCEL, true),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ApiResponse, ClientId, OwnerHandle,
        capabilities::{
            secrets::{MemorySecretStore, SecretKey},
            testing::{ADAPTER, MODULE, TASK, capability_descriptor, temp},
        },
        modules::TestModule,
        redact_request,
    };
    use serde_json::json;
    use std::{
        fs,
        path::{Path, PathBuf},
        thread::JoinHandle,
    };

    fn registry() -> Arc<ModuleRegistry> {
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(TestModule::from_descriptor(capability_descriptor()))
            .unwrap();
        Arc::new(registry)
    }

    /// A catalog, a settings directory that does not exist yet and a file a setting can select, all
    /// under one temporary root that is removed at the end.
    struct Fixture {
        root: PathBuf,
        secrets: Arc<MemorySecretStore>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = temp(name);
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("input.bin"), b"tint").unwrap();
            Self {
                root,
                secrets: Arc::new(MemorySecretStore::new()),
            }
        }

        fn config(&self) -> PathBuf {
            self.root.join("config").join("modules")
        }

        fn start(&self) -> (OwnerHandle, JoinHandle<()>) {
            OwnerHandle::start_with_host(
                &self.root.join("catalog.sqlite"),
                registry(),
                HostConfig {
                    config_dir: Some(self.config()),
                    resource_dir: None,
                    secrets: self.secrets.clone(),
                    ..HostConfig::unconfigured()
                },
            )
            .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn call(
        owner: &OwnerHandle,
        client: ClientId,
        id: &str,
        method: &str,
        params: Value,
    ) -> ApiResponse {
        owner
            .call(
                client,
                ApiRequest {
                    id: id.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(owner: &OwnerHandle, client: ClientId, id: &str, method: &str, params: Value) -> Value {
        let response = call(owner, client, id, method, params);
        assert!(response.error.is_none(), "{id}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn failure(
        owner: &OwnerHandle,
        client: ClientId,
        id: &str,
        method: &str,
        params: Value,
    ) -> (String, String) {
        let error = call(owner, client, id, method, params)
            .error
            .unwrap_or_else(|| panic!("{id} was expected to fail"));
        (error.code, error.message)
    }

    fn mutation(revision: u64, request: &str) -> Value {
        json!({"expected_revision": revision, "request_id": request, "actor": "test"})
    }

    fn stop(owner: OwnerHandle, join: JoinHandle<()>) {
        owner.stop();
        join.join().unwrap();
    }

    /// Every file under `root`, read whole.
    fn files(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut found = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    found.push((path.clone(), fs::read(&path).unwrap()));
                }
            }
        }
        found
    }

    #[test]
    fn discovery_and_reopen_touch_no_settings_file_or_secret_store() {
        let fixture = Fixture::new("discovery");
        let declared = serde_json::to_value(capability_descriptor()).unwrap();
        for round in 0..2 {
            let (owner, join) = fixture.start();
            let client = owner.register();
            let modules = ok(&owner, client, "list", "module.list", json!({}));
            let listed = modules["modules"]
                .as_array()
                .unwrap()
                .iter()
                .find(|module| module["id"] == json!(MODULE))
                .expect("the module is listed");
            assert_eq!(listed, &declared, "round {round}: exactly the declarations");
            let schema = ok(&owner, client, "schema", "schema.list", json!({}));
            let in_schema = schema["modules"]
                .as_array()
                .unwrap()
                .iter()
                .find(|module| module["id"] == json!(MODULE))
                .unwrap();
            assert_eq!(in_schema, &declared);
            let methods = schema["methods"].as_object().unwrap();
            for (method, _) in METHODS {
                assert!(methods.contains_key(*method), "{method} is discoverable");
            }
            assert!(
                !methods.contains_key(&format!("task.{TASK}")),
                "task methods are not generated yet"
            );
            stop(owner, join);
        }
        assert_eq!(
            fixture.secrets.calls().total(),
            0,
            "discovery and reopen never ask the secret store"
        );
        assert!(
            !fixture.root.join("config").exists(),
            "discovery and reopen never create the settings directory"
        );
    }

    #[test]
    fn settings_persist_across_an_owner_restart_and_only_commits_emit_events() {
        let fixture = Fixture::new("restart");
        let input = fixture.root.join("input.bin");
        let (owner, join) = fixture.start();
        let client = owner.register();
        let read = ok(&owner, client, "read", READ, json!({"module_id": MODULE}));
        assert_eq!(read["revision"], json!(0));
        assert_eq!(read["state"], json!("incomplete"), "input-file is required");
        assert_eq!(
            read["fields"]["strength"],
            json!({"value": 0.5, "default": 0.5, "source": "default", "valid": true})
        );
        assert_eq!(
            read["fields"]["token"],
            json!({"secret_present": false, "valid": true})
        );
        assert!(!fixture.config().exists(), "a read creates nothing");
        let set = ok(
            &owner,
            client,
            "set-1",
            SET,
            json!({
                "module_id": MODULE,
                "values": {"mode": "fast", "input-file": input},
                "mutation": mutation(0, "set-1"),
            }),
        );
        assert_eq!(set["outcome"], json!("committed"));
        assert_eq!(set["revision"], json!(1));
        assert_eq!(set["changed"], json!(["input-file", "mode"]));
        assert_eq!(set["invalidates_activation"], json!(true));
        assert_eq!(set["settings"]["state"], json!("ready"));
        assert_eq!(set["settings"]["fields"]["mode"]["value"], json!("fast"));
        let events = ok(
            &owner,
            client,
            "events",
            "events.since",
            json!({"after": 0}),
        );
        assert_eq!(
            events["events"],
            json!([{"sequence": 1, "method": SET, "request_id": "set-1"}])
        );
        // A no-op keeps the revision and emits nothing; a retry returns the original result.
        let again = ok(
            &owner,
            client,
            "set-2",
            SET,
            json!({"module_id": MODULE, "values": {"mode": "fast"}, "mutation": mutation(1, "set-2")}),
        );
        assert_eq!(again["outcome"], json!("no-op"));
        assert_eq!(again["revision"], json!(1));
        let retry = ok(
            &owner,
            client,
            "set-1",
            SET,
            json!({
                "module_id": MODULE,
                "values": {"mode": "fast", "input-file": input},
                "mutation": mutation(0, "set-1"),
            }),
        );
        assert_eq!(retry["deduplicated"], json!(true));
        assert_eq!(retry["revision"], json!(1));
        let events = ok(
            &owner,
            client,
            "events",
            "events.since",
            json!({"after": 0}),
        );
        assert_eq!(events["current_sequence"], json!(1));
        assert_eq!(events["events"].as_array().unwrap().len(), 1);
        let (code, message) = failure(
            &owner,
            client,
            "stale",
            SET,
            json!({"module_id": MODULE, "values": {"mode": "exact"}, "mutation": mutation(0, "stale")}),
        );
        assert_eq!(code, "conflict");
        assert!(
            message.starts_with("stale settings revision 0"),
            "{message}"
        );
        stop(owner, join);
        // A new owner over the same directory reads what the first one committed.
        let (owner, join) = fixture.start();
        let client = owner.register();
        let read = ok(&owner, client, "read", READ, json!({"module_id": MODULE}));
        assert_eq!(read["revision"], json!(1));
        assert_eq!(read["fields"]["mode"]["value"], json!("fast"));
        assert_eq!(read["state"], json!("ready"));
        let (code, _) = failure(
            &owner,
            client,
            "stale",
            SET,
            json!({"module_id": MODULE, "values": {"mode": "exact"}, "mutation": mutation(0, "after")}),
        );
        assert_eq!(code, "conflict");
        stop(owner, join);
    }

    #[test]
    fn profiles_and_secrets_through_the_api_report_presence_only() {
        let fixture = Fixture::new("profiles");
        let (owner, join) = fixture.start();
        let client = owner.register();
        let created = ok(
            &owner,
            client,
            "create",
            CREATE_PROFILE,
            json!({"module_id": MODULE, "adapter": ADAPTER, "label": "Echo", "mutation": mutation(0, "create")}),
        );
        let profile = created["profile"]["id"].as_str().unwrap().to_owned();
        assert_eq!(created["profile"]["adapter"], json!(ADAPTER));
        assert_eq!(
            created["settings"]["profiles"][0]["status"],
            json!("incomplete")
        );
        ok(
            &owner,
            client,
            "endpoint",
            SET,
            json!({
                "module_id": MODULE, "profile_id": profile,
                "values": {"endpoint": "http://localhost:9/echo"}, "mutation": mutation(1, "endpoint"),
            }),
        );
        let secret = ok(
            &owner,
            client,
            "secret",
            SET_SECRET,
            json!({
                "module_id": MODULE, "profile_id": profile, "setting": "api-key",
                "value": "key-value", "mutation": mutation(2, "secret"),
            }),
        );
        assert_eq!(secret["outcome"], json!("committed"));
        let fields = &secret["settings"]["profiles"][0]["fields"];
        assert_eq!(
            fields["api-key"],
            json!({"secret_present": true, "valid": true})
        );
        assert_eq!(
            fields["endpoint"]["value"],
            json!("http://localhost:9/echo")
        );
        assert_eq!(secret["settings"]["profiles"][0]["status"], json!("ready"));
        let key = SecretKey::new(MODULE, Some(&profile), "api-key");
        assert_eq!(
            fixture.secrets.read(&key).unwrap().unwrap().expose(),
            "key-value"
        );
        let cleared = ok(
            &owner,
            client,
            "clear",
            CLEAR_SECRET,
            json!({"module_id": MODULE, "profile_id": profile, "setting": "api-key", "mutation": mutation(3, "clear")}),
        );
        assert_eq!(
            cleared["settings"]["profiles"][0]["status"],
            json!("missing-credentials")
        );
        let removed = ok(
            &owner,
            client,
            "remove",
            REMOVE_PROFILE,
            json!({"module_id": MODULE, "profile_id": profile, "mutation": mutation(4, "remove")}),
        );
        assert_eq!(removed["profile"]["id"], json!(profile));
        assert_eq!(removed["settings"]["profiles"], json!([]));
        // Nothing is left to reset, so a reset is a no-op and announces nothing.
        let reset = ok(
            &owner,
            client,
            "reset",
            RESET,
            json!({"module_id": MODULE, "mutation": mutation(5, "reset")}),
        );
        assert_eq!(reset["outcome"], json!("no-op"));
        assert_eq!(reset["revision"], json!(5));
        let events = ok(
            &owner,
            client,
            "events",
            "events.since",
            json!({"after": 0}),
        );
        let methods: Vec<&str> = events["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| event["method"].as_str().unwrap())
            .collect();
        assert_eq!(
            methods,
            [
                CREATE_PROFILE,
                SET,
                SET_SECRET,
                CLEAR_SECRET,
                REMOVE_PROFILE
            ]
        );
        // A locked store fails the secret write with not-ready and nothing is kept in plain text.
        fixture.secrets.fail_with(Some(Error::new(
            ErrorKind::NotReady,
            "the macOS Keychain is locked",
        )));
        let (code, message) = failure(
            &owner,
            client,
            "locked",
            SET_SECRET,
            json!({"module_id": MODULE, "setting": "token", "value": "locked-plain", "mutation": mutation(5, "locked")}),
        );
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("not-ready", "the macOS Keychain is locked")
        );
        for (path, bytes) in files(&fixture.root) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains("locked-plain"),
                "{} holds the secret",
                path.display()
            );
        }
        stop(owner, join);
    }

    #[test]
    fn settings_methods_refuse_without_a_directory_and_for_unknown_modules() {
        let fixture = Fixture::new("refusals");
        let (owner, join) =
            OwnerHandle::start_with(&fixture.root.join("unconfigured.sqlite"), registry()).unwrap();
        let client = owner.register();
        for (method, params) in [
            (READ, json!({"module_id": MODULE})),
            (
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": "x", "mutation": mutation(0, "a")}),
            ),
            (
                RESET,
                json!({"module_id": MODULE, "mutation": mutation(0, "b")}),
            ),
        ] {
            assert_eq!(
                failure(&owner, client, method, method, params),
                (
                    "not-ready".into(),
                    "no application data directory is configured".into()
                )
            );
        }
        stop(owner, join);
        let (owner, join) = fixture.start();
        let client = owner.register();
        for (params, expected) in [
            (
                json!({"module_id": "test.missing"}),
                "unknown module test.missing",
            ),
            (
                json!({"module_id": "lightwell.basic"}),
                "module lightwell.basic declares no settings",
            ),
            (json!({}), "missing field `module_id`"),
            (
                json!({"module_id": MODULE, "extra": 1}),
                "unknown field `extra`",
            ),
        ] {
            let (code, message) = failure(&owner, client, "read", READ, params);
            assert_eq!(code, "validation");
            assert!(message.contains(expected), "{message}");
        }
        let (code, message) = failure(
            &owner,
            client,
            "wrong",
            SET_SECRET,
            json!({"module_id": MODULE, "setting": "token", "value": 12345, "mutation": mutation(0, "c")}),
        );
        assert_eq!(
            (code.as_str(), message.as_str()),
            ("validation", "value must be a string")
        );
        stop(owner, join);
    }

    #[test]
    fn a_sentinel_secret_appears_on_no_observable_surface() {
        let fixture = Fixture::new("sentinel");
        let sentinel = format!("SENTINEL-{}", uuid::Uuid::new_v4().simple());
        let (owner, join) = fixture.start();
        let client = owner.register();
        let mut observed = Vec::new();
        let mut send = |id: &str, method: &str, params: Value| {
            let response = call(&owner, client, id, method, params);
            observed.push(serde_json::to_string(&response).unwrap());
            response
        };
        let created = send(
            "create",
            CREATE_PROFILE,
            json!({"module_id": MODULE, "adapter": ADAPTER, "label": "Echo", "mutation": mutation(0, "create")}),
        );
        let profile = created.result.unwrap()["profile"]["id"].clone();
        let module_secret = json!({
            "module_id": MODULE, "setting": "token", "value": sentinel, "mutation": mutation(1, "token"),
        });
        assert!(
            send("token", SET_SECRET, module_secret.clone())
                .error
                .is_none()
        );
        assert!(
            send("token", SET_SECRET, module_secret.clone())
                .error
                .is_none(),
            "a retry"
        );
        let profile_secret = json!({
            "module_id": MODULE, "profile_id": profile, "setting": "api-key", "value": sentinel,
            "mutation": mutation(2, "api-key"),
        });
        assert!(send("api-key", SET_SECRET, profile_secret).error.is_none());
        // Refused requests carrying the secret echo none of it.
        for (id, method, params) in [
            (
                "long",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": format!("{sentinel}{}", "x".repeat(64)), "mutation": mutation(3, "long")}),
            ),
            (
                "object",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": {"nested": sentinel}, "mutation": mutation(3, "object")}),
            ),
            (
                "array",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": [sentinel], "mutation": mutation(3, "array")}),
            ),
            (
                "extra",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": sentinel, "extra": sentinel, "mutation": mutation(3, "extra")}),
            ),
            (
                "wrong-setting",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "note", "value": sentinel, "mutation": mutation(3, "wrong")}),
            ),
            (
                "as-value",
                SET,
                json!({"module_id": MODULE, "values": {"token": sentinel}, "mutation": mutation(3, "as-value")}),
            ),
            (
                "clear-with-value",
                CLEAR_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": sentinel, "mutation": mutation(3, "clear")}),
            ),
            (
                "stale",
                SET_SECRET,
                json!({"module_id": MODULE, "setting": "token", "value": sentinel, "mutation": mutation(0, "stale")}),
            ),
        ] {
            assert!(send(id, method, params).error.is_some(), "{id} was refused");
        }
        send("read", READ, json!({"module_id": MODULE}));
        send("events", "events.since", json!({"after": 0}));
        send("list", "module.list", json!({}));
        send("schema", "schema.list", json!({}));
        send("status", "session.state", json!({}));
        stop(owner, join);
        assert!(observed.len() >= 14);
        for text in &observed {
            assert!(
                !text.contains(&sentinel),
                "a response carries the secret: {text}"
            );
        }
        for (path, bytes) in files(&fixture.root) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains(&sentinel),
                "{} holds the secret",
                path.display()
            );
        }
        // The secret did reach the store, under both of its keys.
        for key in [
            SecretKey::new(MODULE, None, "token"),
            SecretKey::new(MODULE, profile.as_str(), "api-key"),
        ] {
            assert_eq!(
                fixture.secrets.read(&key).unwrap().unwrap().expose(),
                sentinel
            );
        }
        // And a request log, evidence capture or Copy as JSON of the request carries none of it.
        let request = ApiRequest {
            id: "token".into(),
            method: SET_SECRET.into(),
            params: module_secret,
            token: None,
        };
        assert!(serde_json::to_string(&request).unwrap().contains(&sentinel));
        let redacted = serde_json::to_string(&redact_request(&request)).unwrap();
        assert!(!redacted.contains(&sentinel), "{redacted}");
        assert!(redacted.contains("<redacted>"));
    }
}
