//! The capability host the catalog owner holds: where settings, grants and resources live, which
//! secret store holds credentials, the capability worker's lanes and jobs, each module's
//! activation, and the operations the owner-answered `module.*` methods call over all of them. The
//! method table in `api::methods` names each method, parses its parameters, declared beside the
//! operation, and calls one of these operations; nothing here dispatches by method name. Every call
//! is a short file, stat or secret-store attribute call; nothing hashes, downloads, loads module
//! state or reads a secret's data on the owner. That work is queued on a lane, whose result comes
//! back into the owner's channel. See `docs/design/module-capabilities.md`.
//!
//! This file holds the host itself, the settings methods, the capability jobs and `module.status`;
//! [`activation`], [`permissions`], [`resources`] and [`tasks`] hold the other methods over the
//! same [`CapabilityHost`].
use super::{
    descriptor::SettingDescriptor,
    grants::{Grant, GrantKind, GrantScope, GrantsStore},
    jobs::{Cancelled, Deliver, JobError, JobKind, JobRecord, JobStatus, Jobs, Origin},
    resources::{DEFAULT_RESOURCE_QUOTA_BYTES, ResourceStore, SharedTransport},
    secrets::{SecretStore, SecretValue, UnavailableSecretStore},
    settings::{
        CLEAR_SECRET, CREATE_PROFILE, FieldRead, REMOVE_PROFILE, RESET, SET, SET_SECRET,
        SettingsRead, SettingsState, SettingsStore, SettingsWrite, WriteOutcome,
    },
    transport::{Endpoint, TransportConfig, parse_endpoint},
};
use crate::{
    AssetId, EditorService, Error, ErrorKind, JobId, ModuleDescriptor, ModuleRegistry,
    ParameterKind, activity::ActivityBoard, api::params::host_params,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, path::PathBuf, sync::Arc};

mod activation;
mod permissions;
mod resources;
mod tasks;

use activation::Activation;
pub(crate) use activation::ModuleChange;
pub use activation::{ActivationRead, ActivationState};
pub(crate) use permissions::{DenyParams, GrantParams, PermissionList, RevokeParams};
pub(crate) use resources::{InstallParams, ResourceParams};
pub use tasks::TASK_PREFIX;

/// The lifecycle methods.
pub const ACTIVATE: &str = "module.activate";
pub const DEACTIVATE: &str = "module.deactivate";
pub const STATUS: &str = "module.status";

/// Why grants are revoked or a module deactivated when something they depend on changes.
pub const ENDPOINT_CHANGED: &str = "endpoint changed";
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

fn encode(value: impl serde::Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
}

/// Announce `origin` once, however many changes a request made.
pub(crate) fn announce_once(announce: &mut Vec<Origin>, origin: &Origin) {
    if !announce.contains(origin) {
        announce.push(origin.clone());
    }
}

host_params! {
    /// `module.settings.read`, `module.status` and `module.resource.list`.
    pub(crate) struct ModuleParams {
        module_id: String,
    }
}

host_params! {
    pub(crate) struct SetParams {
        module_id: String,
        values: Map<String, Value>,
        mutation: Mutation,
        profile_id: Option<String> = "the profile whose fields to set; default the module's own fields",
    }
}

host_params! {
    /// `set-secret`. Its value is read by [`SecretParam`], which never echoes it.
    pub(crate) struct SetSecretParams {
        module_id: String,
        setting: String,
        value: SecretParam,
        mutation: Mutation,
        profile_id: Option<String> = "the profile whose secret to set; default the module's own",
    }
}

host_params! {
    pub(crate) struct ClearSecretParams {
        module_id: String,
        setting: String,
        mutation: Mutation,
        profile_id: Option<String> = "the profile whose secret to clear; default the module's own",
    }
}

host_params! {
    pub(crate) struct ResetParams {
        module_id: String,
        mutation: Mutation,
    }
}

host_params! {
    pub(crate) struct CreateProfileParams {
        module_id: String,
        adapter: String,
        label: String,
        mutation: Mutation,
    }
}

host_params! {
    pub(crate) struct RemoveProfileParams {
        module_id: String,
        profile_id: String,
        mutation: Mutation,
    }
}

host_params! {
    /// `module.job.read`.
    pub(crate) struct JobParams {
        job_id: JobId,
    }
}

host_params! {
    /// `module.job.cancel`.
    pub(crate) struct JobCancelParams {
        job_id: JobId,
        mutation: MutationRequest,
    }
}

/// The `value` of a `set-secret` request, moved straight into a [`SecretValue`]. A malformed one is
/// refused without echoing it, which serde's own message for a wrong type would do.
pub(crate) struct SecretParam(SecretValue);

impl<'de> Deserialize<'de> for SecretParam {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Secret;
        fn refused<E: serde::de::Error>() -> E {
            E::custom("value must be a string")
        }
        impl<'de> serde::de::Visitor<'de> for Secret {
            type Value = SecretParam;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<SecretParam, E> {
                Ok(SecretParam(SecretValue::new(value.to_owned())))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<SecretParam, E> {
                Ok(SecretParam(SecretValue::new(value)))
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<SecretParam, E> {
                Err(refused())
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<SecretParam, E> {
                Err(refused())
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<SecretParam, E> {
                Err(refused())
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<SecretParam, E> {
                Err(refused())
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<SecretParam, E> {
                Err(refused())
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<SecretParam, E> {
                Err(refused())
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                _: A,
            ) -> Result<SecretParam, A::Error> {
                Err(refused())
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                _: A,
            ) -> Result<SecretParam, A::Error> {
                Err(refused())
            }
        }
        deserializer.deserialize_any(Secret)
    }
}

/// One unmet requirement of an activation or a task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirement {
    /// `setting`, `resource`, or for a task also `activation` and `profile`.
    pub kind: String,
    /// The setting, resource, module or profile identity.
    pub id: String,
    /// `missing`, `invalid`, `incompatible` or `unavailable` for a setting; a resource state for a
    /// resource; the module's activation state for an activation; a profile status for a profile.
    pub state: String,
}

/// The owner's capability state: the configuration, the stores over its directories, the lanes
/// and their jobs, each module's activation and what each live task reports back.
pub(crate) struct CapabilityHost {
    config: HostConfig,
    settings: Option<SettingsStore>,
    grants: Option<GrantsStore>,
    resources: Option<ResourceStore>,
    transport: Arc<SharedTransport>,
    jobs: Jobs,
    activations: HashMap<String, Activation>,
    /// Queued and running tasks, at most one lane's worth.
    tasks: HashMap<JobId, tasks::TaskRun>,
}

impl CapabilityHost {
    /// `deliver` posts a finished job into the owner's channel; it is called on a lane thread.
    /// `board` is the owner's activity board, which every capability job publishes to while it
    /// runs, beside source preparation and analysis.
    pub(crate) fn new(config: HostConfig, deliver: Deliver, board: Arc<ActivityBoard>) -> Self {
        Self {
            settings: config.config_dir.clone().map(SettingsStore::new),
            grants: config.config_dir.clone().map(GrantsStore::new),
            resources: config.resource_dir.clone().map(ResourceStore::new),
            transport: Arc::new(SharedTransport::new(config.transport.clone())),
            jobs: Jobs::new(deliver, board),
            activations: HashMap::new(),
            tasks: HashMap::new(),
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

    /// A lane finished a job: record it, update the module it belongs to, and announce what
    /// changed under the request that started it. A task's artifacts are recorded in the catalog
    /// first, so a client that reads it succeeded can apply them. A failed install, removal or
    /// task changes nothing a client shows, so it announces nothing.
    pub(crate) fn finished(
        &mut self,
        service: &mut EditorService,
        job_id: &JobId,
        result: Result<Value, Error>,
        announce: &mut Vec<Origin>,
    ) {
        let result = self.task_result(service, job_id, result);
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
                if done.record.status == JobStatus::Ready
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

    /// `module.settings.read`.
    pub(crate) fn read(
        &self,
        registry: &ModuleRegistry,
        request: ModuleParams,
    ) -> Result<Value, Error> {
        let descriptor = module(registry, &request.module_id)?;
        encode(self.settings()?.read(descriptor, self.secrets())?)
    }

    /// `module.settings.set`.
    pub(crate) fn settings_set(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: SetParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let descriptor = module(registry, &request.module_id)?;
        let write = self.settings()?.set(
            descriptor,
            request.profile_id.as_deref(),
            &request.values,
            &request.mutation,
        )?;
        self.settings_written(registry, descriptor, SET, write, origin, announce)
    }

    /// `module.settings.set-secret`: the value was moved into a [`SecretValue`] as it was parsed.
    pub(crate) fn settings_set_secret(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: SetSecretParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let descriptor = module(registry, &request.module_id)?;
        let write = self.settings()?.set_secret(
            descriptor,
            self.secrets(),
            request.profile_id.as_deref(),
            &request.setting,
            &request.value.0,
            &request.mutation,
        )?;
        self.settings_written(registry, descriptor, SET_SECRET, write, origin, announce)
    }

    /// `module.settings.clear-secret`.
    pub(crate) fn settings_clear_secret(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: ClearSecretParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let descriptor = module(registry, &request.module_id)?;
        let write = self.settings()?.clear_secret(
            descriptor,
            self.secrets(),
            request.profile_id.as_deref(),
            &request.setting,
            &request.mutation,
        )?;
        self.settings_written(registry, descriptor, CLEAR_SECRET, write, origin, announce)
    }

    /// `module.settings.reset`.
    pub(crate) fn settings_reset(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: ResetParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let descriptor = module(registry, &request.module_id)?;
        let write = self
            .settings()?
            .reset(descriptor, self.secrets(), &request.mutation)?;
        self.settings_written(registry, descriptor, RESET, write, origin, announce)
    }

    /// `module.profile.create`.
    pub(crate) fn profile_create(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: CreateProfileParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let descriptor = module(registry, &request.module_id)?;
        let write = self.settings()?.create_profile(
            descriptor,
            &request.adapter,
            &request.label,
            &request.mutation,
        )?;
        self.settings_written(
            registry,
            descriptor,
            CREATE_PROFILE,
            write,
            origin,
            announce,
        )
    }

    /// `module.profile.remove`.
    pub(crate) fn profile_remove(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: RemoveProfileParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let descriptor = module(registry, &request.module_id)?;
        let write = self.settings()?.remove_profile(
            descriptor,
            self.secrets(),
            &request.profile_id,
            &request.mutation,
        )?;
        self.settings_written(
            registry,
            descriptor,
            REMOVE_PROFILE,
            write,
            origin,
            announce,
        )
    }

    /// Every settings write: apply what a committed one implies for grants and activation, and
    /// answer with its result and the module's settings as they read now. Neither holds a secret. A
    /// retry never gets here: the owner's request table answers it with the first answer.
    fn settings_written(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        descriptor: &ModuleDescriptor,
        method: &str,
        write: SettingsWrite,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let mut value = encode(&write.result)?;
        if write.result.outcome == WriteOutcome::Committed {
            announce_once(announce, origin);
            // The write is committed whatever follows, so a grants file that cannot be updated is
            // reported beside the result rather than as the write's failure. Nothing it leaves
            // behind can be used: a grant names its exact path or endpoint origin, which the
            // settings no longer hold.
            match self.after_settings_write(registry, descriptor, method, &write, origin, announce)
            {
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

    /// What a committed settings write implies. A changed field declared `invalidates_activation`
    /// deactivates an active or activating module. Grants scoped to a value the write replaced or
    /// removed are revoked, and the jobs running under them are cancelled: a profile's remote
    /// grants when its endpoint changes or it is removed, and a reset's remote grants. Download
    /// grants name a pinned resource, not a setting, and survive a reset.
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
            SET => {
                if let Some(profile) = &result.profile_id {
                    let endpoint = result.changed.iter().any(|id| {
                        settings
                            .profiles
                            .as_ref()
                            .and_then(|profiles| profiles.field(id))
                            .is_some_and(SettingDescriptor::is_endpoint)
                    });
                    if endpoint {
                        revoked.extend(grants.revoke_matching(
                            |grant| remote_of(profile, grant),
                            ENDPOINT_CHANGED,
                        )?);
                    }
                }
            }
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
                            && matches!(grant.kind, GrantKind::RemoteImageRequest)
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

    // Jobs.

    /// `module.job.read`.
    pub(crate) fn job_read(&self, request: JobParams) -> Result<Value, Error> {
        encode(
            self.jobs
                .read(&request.job_id)
                .ok_or_else(|| unknown_job(&request.job_id))?,
        )
    }

    /// `module.job.cancel`: any client may, since a capability job belongs to its module. A
    /// deactivation releases what a module holds and is never cancelled.
    pub(crate) fn job_cancel(
        &mut self,
        request: JobCancelParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        request.mutation.validate()?;
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
                // A task removed before it ran never reports back.
                if record.kind == JobKind::Task {
                    self.forget_task(&record.job_id);
                }
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

    // Status.

    /// `module.status`: activation, settings validity, resources, grants and this module's jobs.
    /// A settings read, stats of installed markers and a grants file read; no hashing.
    pub(crate) fn status(
        &self,
        registry: &ModuleRegistry,
        request: ModuleParams,
    ) -> Result<Value, Error> {
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

/// The valid, non-null, non-secret module-level values of a settings read that a module may read
/// as values. An endpoint is left out: a job reaches it only through the capability that names it,
/// so a module never holds a URL of its own.
fn effective_values(
    descriptor: &ModuleDescriptor,
    read: Option<&SettingsRead>,
) -> Map<String, Value> {
    let plain = |id: &str| {
        descriptor
            .settings
            .as_ref()
            .and_then(|settings| settings.field(id))
            .is_some_and(|field| !field.is_endpoint())
    };
    read.map(|read| {
        read.fields
            .iter()
            .filter(|(id, _)| plain(id))
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
        .filter(|field| field.is_secret())
        .map(|field| field.id().to_owned())
        .collect()
}

/// The endpoint a profile's first endpoint field names, when it holds a valid value.
fn profile_endpoint(
    descriptor: &ModuleDescriptor,
    fields: &std::collections::BTreeMap<String, FieldRead>,
) -> Option<Endpoint> {
    let profiles = descriptor.settings.as_ref()?.profiles.as_ref()?;
    let (field, classes) = profiles
        .fields
        .iter()
        .find_map(|field| match field.kind() {
            ParameterKind::Endpoint { classes } => Some((field, classes)),
            _ => None,
        })?;
    match fields.get(field.id())? {
        FieldRead::Value {
            value: Value::String(url),
            valid: true,
            ..
        } => parse_endpoint(url, classes).ok(),
        _ => None,
    }
}

/// The origin a profile's first endpoint field sends to, when it holds a valid value.
fn profile_origin(
    descriptor: &ModuleDescriptor,
    fields: &std::collections::BTreeMap<String, FieldRead>,
) -> Option<String> {
    profile_endpoint(descriptor, fields).map(|endpoint| endpoint.origin())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ApiRequest, ApiResponse, ClientId, OwnerHandle,
        capabilities::{
            grants::{DENY, GRANT, LIST, REVOKE},
            jobs::{JOB_CANCEL, JOB_READ},
            resources::{INSTALL, REMOVE, RESOURCE_LIST},
            secrets::{MemorySecretStore, SecretKey},
            settings::READ,
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

    /// A catalog and a settings directory that does not exist yet, under one temporary root that is
    /// removed at the end.
    struct Fixture {
        root: PathBuf,
        secrets: Arc<MemorySecretStore>,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let root = temp(name);
            fs::create_dir_all(&root).unwrap();
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
            for method in [
                READ,
                SET,
                SET_SECRET,
                CLEAR_SECRET,
                RESET,
                CREATE_PROFILE,
                REMOVE_PROFILE,
                GRANT,
                DENY,
                REVOKE,
                LIST,
                ACTIVATE,
                DEACTIVATE,
                STATUS,
                RESOURCE_LIST,
                INSTALL,
                REMOVE,
                JOB_READ,
                JOB_CANCEL,
            ] {
                assert!(methods.contains_key(method), "{method} is discoverable");
            }
            let task = &methods[&format!("task.{TASK}")];
            assert_eq!(task["mutates"], json!(true), "the request queues a job");
            assert_eq!(task["mutation"], json!("request"));
            assert_eq!(
                task["required"],
                json!(["mutation", "asset_id", "profile_id"])
            );
            assert_eq!(task["optional"], json!({"gain": "test"}));
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
        let (owner, join) = fixture.start();
        let client = owner.register();
        let read = ok(&owner, client, "read", READ, json!({"module_id": MODULE}));
        assert_eq!(read["revision"], json!(0));
        assert_eq!(read["state"], json!("incomplete"), "label is required");
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
                "values": {"mode": "fast", "label": "tint"},
                "mutation": mutation(0, "set-1"),
            }),
        );
        assert_eq!(set["outcome"], json!("committed"));
        assert_eq!(set["revision"], json!(1));
        assert_eq!(set["changed"], json!(["label", "mode"]));
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
                "values": {"mode": "fast", "label": "tint"},
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
        // The same request_id with other input is a conflict, within the method family.
        let (code, message) = failure(
            &owner,
            client,
            "set-1",
            SET,
            json!({"module_id": MODULE, "values": {"mode": "exact"}, "mutation": mutation(0, "set-1")}),
        );
        assert_eq!(
            (code.as_str(), message.as_str()),
            (
                "conflict",
                "request_id was already used with different input"
            )
        );
        // A secret write's retry is matched by the setting alone: the value is never part of the
        // request's identity, and the retry stores nothing.
        let secret = |value: &str| json!({"module_id": MODULE, "setting": "token", "value": value, "mutation": mutation(1, "secret")});
        let first = ok(&owner, client, "secret", SET_SECRET, secret("first-value"));
        assert_eq!(first["deduplicated"], json!(false));
        let again = ok(&owner, client, "secret", SET_SECRET, secret("other-value"));
        assert_eq!(again["deduplicated"], json!(true));
        assert_eq!(again["revision"], json!(2));
        let key = SecretKey::new(MODULE, None, "token");
        assert_eq!(
            fixture.secrets.read(&key).unwrap().unwrap().expose(),
            "first-value"
        );
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
        // A new owner over the same directory reads what the first one committed. The request
        // table went with the first owner, so a retry now conflicts on the revision it was made
        // against, and the client reads the settings its write left.
        let (owner, join) = fixture.start();
        let client = owner.register();
        let read = ok(&owner, client, "read", READ, json!({"module_id": MODULE}));
        assert_eq!(read["revision"], json!(2));
        assert_eq!(read["fields"]["mode"]["value"], json!("fast"));
        assert_eq!(read["state"], json!("ready"));
        let (code, message) = failure(
            &owner,
            client,
            "set-1",
            SET,
            json!({
                "module_id": MODULE,
                "values": {"mode": "fast", "label": "tint"},
                "mutation": mutation(0, "set-1"),
            }),
        );
        assert_eq!(code, "conflict");
        assert!(
            message.starts_with("stale settings revision 0"),
            "{message}"
        );
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
