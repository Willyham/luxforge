//! Activation, the `module.activate` and `module.deactivate` methods: a module's activation state,
//! the check of its declared requirements, the activation job on the module lane and the release of
//! what an active module loaded. See `docs/design/module-capabilities.md#lifecycle-jobs-and-resources`.
use super::{
    CapabilityHost, Requirement, announce_once, effective_values, registered, secret_fields,
    setting_requirement, validation,
};
use crate::{
    Availability, Error, ErrorKind, JobId, ModuleRegistry,
    api::params::host_params,
    capabilities::{
        context::ModuleContext,
        jobs::{
            Admission, Cancelled, JobControl, JobError, JobKind, JobRecord, JobStatus, NewJob,
            Origin, Work,
        },
        resources::ResourceState,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    panic::{self, AssertUnwindSafe},
    sync::Arc,
};

host_params! {
    /// `module.activate` and `module.deactivate`.
    pub(crate) struct ModuleChange {
        module_id: String,
        mutation: MutationRequest,
    }
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

#[derive(Default)]
pub(super) struct Activation {
    pub(super) state: ActivationState,
    pub(super) reason: Option<String>,
    pub(super) error: Option<JobError>,
    /// The activation job while activating; the release job of a deactivation while it runs.
    pub(super) job: Option<JobId>,
    /// A deactivation asked for while the activation job ran, with its reason: whatever the job
    /// does, the module ends inactive and anything it loaded is released.
    pub(super) pending: Option<Option<String>>,
}

impl Activation {
    pub(super) fn read(&self) -> ActivationRead {
        ActivationRead {
            state: self.state,
            reason: self.reason.clone(),
            job_id: self.job.clone(),
            error: self.error.clone(),
        }
    }

    pub(super) fn set_inactive(&mut self, reason: Option<String>) {
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

impl CapabilityHost {
    /// `module.activate`: check every declared requirement and queue the activation on the module
    /// lane, or join the one already queued or running. An active module answers at once.
    pub(crate) fn activate(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: ModuleChange,
        origin: &Origin,
    ) -> Result<Value, Error> {
        request.mutation.validate()?;
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
                    effective_values(descriptor, settings.as_ref()),
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
                let module = registry.capabilities(&module_id).ok_or_else(|| {
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
    pub(crate) fn deactivate_request(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: ModuleChange,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        request.mutation.validate()?;
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
    pub(super) fn deactivate(
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
            if let Some(module) = registry.capabilities(&owned) {
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
    pub(super) fn activation_finished(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        record: &JobRecord,
    ) -> bool {
        let Some(activation) = self.activations.get_mut(&record.module_id) else {
            return false;
        };
        if activation.job.as_ref() != Some(&record.job_id) {
            return false;
        }
        activation.job = None;
        let pending = activation.pending.take();
        match (record.status, pending) {
            (JobStatus::Ready, None) => {
                activation.state = ActivationState::Active;
                activation.reason = None;
                activation.error = None;
            }
            // It finished loading just as it was asked to stop: release what it loaded.
            (JobStatus::Ready, Some(reason)) => {
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
}
