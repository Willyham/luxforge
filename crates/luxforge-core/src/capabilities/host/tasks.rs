//! Worker tasks, the generated `task.<id>` methods. Before anything is queued the owner checks the
//! module, the parameters, the asset and profile, the activation, every requirement and a live
//! grant for each capability the task uses, and binds the data the task may send: for
//! `sample-grid-8`, the asset's current entry with its verified source and artifacts, compiled once
//! in `O(layers)`. The task then runs on the module lane under those grants, so revoking one cancels
//! it; its 64 point samples are taken there, from what was bound, the first time it sends. When it
//! succeeds the owner records what it published before the job reads succeeded; a failed or
//! cancelled task commits nothing. See
//! `docs/design/module-capabilities.md#lifecycle-jobs-and-resources`.
use super::{
    ActivationState, CapabilityHost, Requirement, asset_exists, effective_values, profile_endpoint,
    secret_fields,
};
use crate::{
    AssetId, Availability, EditorService, Error, JobId, ModuleDescriptor, MutationRequest,
    api::params,
    capabilities::{
        consent::{consent_required, remote_disclosure},
        context::{GrantedSend, ModuleContext, ProfileView, TaskOutcome},
        data::DisclosedData,
        descriptor::{
            AdapterAuth, AdapterDescriptor, CapabilityDescriptor, CapabilityKind, DataClass,
        },
        grants::{GrantScope, RemoteScope},
        jobs::{Admission, JobControl, JobKind, JobStatus, NewJob, Origin, Work},
        resources::ResourceState,
        settings::{FieldRead, ProfileRead, ProfileStatus},
        transport::Endpoint,
    },
    modules::check_declared_values,
};
use serde_json::{Value, json};
use std::sync::Arc;

/// Every generated task method is this prefix and the task's identity: task `generate-proof-tint`
/// is `task.generate-proof-tint`.
pub const TASK_PREFIX: &str = "task.";

/// What the owner keeps of a queued or running task: its control, to tell a result that arrived
/// after a revocation from one that did not, and what its worker reports back.
pub(super) struct TaskRun {
    control: Arc<JobControl>,
    outcome: Arc<TaskOutcome>,
}

/// One capability a task uses, with the scope its live grant names and, for a remote request, the
/// profile's endpoint that scope's origin was taken from.
struct Granted<'a> {
    capability: &'a CapabilityDescriptor,
    scope: GrantScope,
    endpoint: Option<Endpoint>,
    grant_id: String,
}

/// A declared adapter of a module's profile block.
fn adapter<'a>(descriptor: &'a ModuleDescriptor, id: &str) -> Result<&'a AdapterDescriptor, Error> {
    descriptor
        .settings
        .as_ref()
        .and_then(|settings| settings.profiles.as_ref())
        .and_then(|profiles| profiles.adapter(id))
        .ok_or_else(|| {
            Error::internal(format!(
                "adapter {id} of module {} is not declared",
                descriptor.id
            ))
        })
}

fn profile_status(status: ProfileStatus) -> &'static str {
    match status {
        ProfileStatus::Ready => "ready",
        ProfileStatus::Incomplete => "incomplete",
        ProfileStatus::MissingCredentials => "missing-credentials",
        ProfileStatus::Incompatible => "incompatible",
    }
}

/// A profile as a task sees it: its non-secret values without its endpoint, which only the host
/// sends to.
fn profile_view(descriptor: &ModuleDescriptor, profile: &ProfileRead) -> ProfileView {
    let fields = descriptor
        .settings
        .as_ref()
        .and_then(|settings| settings.profiles.as_ref());
    let plain = |id: &str| {
        fields
            .and_then(|profiles| profiles.field(id))
            .is_some_and(|field| !field.kind().setting_only())
    };
    ProfileView {
        id: profile.id.clone(),
        adapter: profile.adapter.clone(),
        values: profile
            .fields
            .iter()
            .filter(|(id, _)| plain(id))
            .filter_map(|(id, field)| match field {
                FieldRead::Value {
                    value, valid: true, ..
                } if !value.is_null() => Some((id.clone(), value.clone())),
                _ => None,
            })
            .collect(),
    }
}

/// The secret fields of a module's profile block.
fn profile_secret_fields(descriptor: &ModuleDescriptor) -> Vec<String> {
    descriptor
        .settings
        .iter()
        .flat_map(|settings| settings.profiles.iter())
        .flat_map(|profiles| profiles.fields.iter())
        .filter(|field| field.is_secret())
        .map(|field| field.id().to_owned())
        .collect()
}

impl CapabilityHost {
    /// `task.<id>`: check, in order, that the module is available, the `{request_id, actor}`
    /// envelope, the parameters, the asset, the profile's readiness, every requirement (listed
    /// together as `not-ready`), and a live grant for each capability the task uses (the first
    /// missing one is `consent-required`); then bind the disclosed data, which is
    /// `preparation-required` for an unprepared source or artifact, and queue the task on the module
    /// lane under its grants. Returns `{job_id, status}`. A retry never gets here: the owner's
    /// request table answers it with the first job, so a retried task starts nothing.
    pub(crate) fn task(
        &mut self,
        service: &EditorService,
        task_id: &str,
        request: &Value,
        origin: &Origin,
    ) -> Result<Value, Error> {
        let registry = service.registry();
        let (module, task) = registry
            .task(task_id)
            .ok_or_else(|| Error::validation(format!("unknown task {task_id}")))?;
        let descriptor = module.descriptor();
        let module_id = descriptor.id.as_str();
        if let Availability::Unavailable { reason } = &descriptor.availability {
            return Err(Error::incompatible(format!(
                "unavailable module {module_id} cannot run task {task_id}: {reason}"
            )));
        }
        let mut parameters = params::generated(request)?;
        params::take::<MutationRequest>(&mut parameters, "mutation")?.validate()?;
        let asset_id: Option<AssetId> = task
            .asset
            .then(|| params::take(&mut parameters, "asset_id"))
            .transpose()?;
        let profile_id: Option<String> = task
            .profile
            .then(|| params::take(&mut parameters, "profile_id"))
            .transpose()?;
        let checked = check_declared_values(
            "task",
            &task.id,
            &task.parameters,
            false,
            &Value::Object(parameters),
        )?;
        if let Some(asset_id) = &asset_id {
            asset_exists(service, asset_id)?;
        }
        let settings = match &descriptor.settings {
            Some(_) => Some(self.settings()?.read(descriptor, self.secrets())?),
            None => None,
        };
        let profile = match (&profile_id, &settings) {
            (Some(profile_id), Some(settings)) => Some(
                settings
                    .profile(profile_id)
                    .ok_or_else(|| Error::validation(format!("unknown profile {profile_id}")))?,
            ),
            (Some(_), None) => {
                return Err(Error::internal(format!(
                    "task {task_id} takes a profile but its module declares none"
                )));
            }
            (None, _) => None,
        };
        let capabilities = task
            .uses
            .iter()
            .map(|id| {
                descriptor.capability(id).ok_or_else(|| {
                    Error::internal(format!("task {task_id} uses undeclared capability {id}"))
                })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        if let Some(profile) = profile {
            for capability in &capabilities {
                if let CapabilityKind::RemoteImageRequest { adapter, .. } = &capability.kind
                    && profile.adapter != *adapter
                {
                    return Err(Error::validation(format!(
                        "profile {} uses adapter {}, not {adapter}",
                        profile.id, profile.adapter
                    )));
                }
            }
        }

        // Every unmet requirement, listed together before anything else is asked.
        let mut missing = Vec::new();
        if let Some(profile) = profile
            && profile.status != ProfileStatus::Ready
        {
            missing.push(Requirement {
                kind: "profile".into(),
                id: profile.id.clone(),
                state: profile_status(profile.status).into(),
            });
        }
        if task.requires_active {
            let state = self
                .activations
                .get(module_id)
                .map_or(ActivationState::Inactive, |activation| activation.state);
            if state != ActivationState::Active {
                missing.push(Requirement {
                    kind: "activation".into(),
                    id: module_id.to_owned(),
                    state: state.name().into(),
                });
            }
        }
        let rows = self.resource_rows(descriptor);
        let mut resources = Vec::new();
        for capability in &capabilities {
            match &capability.kind {
                // A task that uses a resource's capability reads the installed resource; the
                // install asked for the download grant.
                CapabilityKind::DownloadArtifact { resource } => {
                    match rows.iter().find(|row| &row.id == resource) {
                        Some(row) if row.state == ResourceState::Installed => {
                            if let Some(path) = &row.path {
                                resources.push((resource.clone(), path.clone()));
                            }
                        }
                        row => missing.push(Requirement {
                            kind: "resource".into(),
                            id: resource.clone(),
                            state: row
                                .map_or(ResourceState::NotInstalled, |row| row.state)
                                .name()
                                .into(),
                        }),
                    }
                }
                CapabilityKind::RemoteImageRequest { .. } => {}
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
            return Err(
                Error::not_ready(format!("task {task_id} is not ready: {list}"))
                    .with_data(json!({"requirements": missing})),
            );
        }

        // A live grant for every gated capability, in the order the task declares them.
        let mut granted = Vec::new();
        for capability in &capabilities {
            let (scope, endpoint) = match &capability.kind {
                CapabilityKind::RemoteImageRequest { adapter, data } => {
                    let (Some(profile), Some(asset_id)) = (profile, &asset_id) else {
                        return Err(Error::internal(format!(
                            "task {task_id} sends without an asset and a profile"
                        )));
                    };
                    let endpoint =
                        profile_endpoint(descriptor, &profile.fields).ok_or_else(|| {
                            Error::validation(format!(
                                "profile {} has no valid endpoint",
                                profile.id
                            ))
                        })?;
                    let scope = GrantScope::Remote(RemoteScope {
                        profile_id: profile.id.clone(),
                        adapter: adapter.clone(),
                        origin: endpoint.origin(),
                        data: *data,
                        asset_id: asset_id.clone(),
                    });
                    (scope, Some(endpoint))
                }
                CapabilityKind::DownloadArtifact { .. } => continue,
            };
            let (grant, denied) = self.grants()?.consent(module_id, &capability.id, &scope)?;
            let Some(grant) = grant else {
                let disclosure = match &scope {
                    GrantScope::Remote(scope) => remote_disclosure(
                        descriptor,
                        capability,
                        adapter(descriptor, &scope.adapter)?,
                        scope,
                    ),
                    GrantScope::Download(_) => {
                        return Err(Error::internal("a task never asks for a download grant"));
                    }
                };
                return Err(consent_required(
                    descriptor, capability, &scope, disclosure, denied,
                ));
            };
            granted.push(Granted {
                capability,
                scope,
                endpoint,
                grant_id: grant.grant_id,
            });
        }

        let run = TaskRun {
            control: JobControl::new(),
            outcome: Arc::new(TaskOutcome::default()),
        };
        let mut context =
            ModuleContext::new(module_id, self.config.secrets.clone(), run.control.clone())
                .with_settings(
                    effective_values(descriptor, settings.as_ref()),
                    secret_fields(descriptor),
                )
                .with_artifacts(service.artifact_writer()?, run.outcome.clone());
        if let Some(profile) = profile {
            context = context.with_profile(
                profile_view(descriptor, profile),
                profile_secret_fields(descriptor),
            );
        }
        for (resource, path) in resources {
            context = context.with_resource(&resource, path);
        }
        let mut grant_ids = Vec::with_capacity(granted.len());
        for Granted {
            capability,
            scope,
            endpoint,
            grant_id,
        } in granted
        {
            grant_ids.push(grant_id);
            context = match (scope, endpoint) {
                (GrantScope::Remote(scope), Some(endpoint)) => {
                    // The disclosed data is the asset's current entry as it is now, bound here and
                    // sampled on the worker: a point through a spatial layer evaluates a whole
                    // tile, which 64 samples must not cost the owner.
                    let data = match scope.data {
                        DataClass::SampleGrid8 => {
                            DisclosedData::new(scope.data, service.sample_plan(&scope.asset_id)?)
                        }
                    };
                    let adapter = adapter(descriptor, &scope.adapter)?.clone();
                    let credential = (adapter.auth == AdapterAuth::Bearer)
                        .then(|| profile_secret_fields(descriptor).into_iter().next())
                        .flatten();
                    context.with_send(
                        &capability.id,
                        GrantedSend {
                            transport: self.transport.clone(),
                            endpoint,
                            adapter,
                            credential,
                            data,
                        },
                    )
                }
                _ => {
                    return Err(Error::internal("a granted scope has no request to send"));
                }
            };
        }
        let work: Work = {
            let registry = registry.clone();
            let module_id = module_id.to_owned();
            let task_id = task_id.to_owned();
            let outcome = run.outcome.clone();
            Box::new(move || {
                let module = registry.capabilities(&module_id).ok_or_else(|| {
                    Error::internal(format!("module {module_id} is not registered"))
                })?;
                let result = module.run_task(&task_id, &checked, &context)?;
                // A task asked to stop as it finished counts as cancelled: what it published is
                // not recorded.
                context.checkpoint()?;
                Ok(json!({"result": result, "artifacts": outcome.published_ids()}))
            })
        };
        // Only live tasks are kept; a queued task removed by its lane never reports back.
        self.tasks.retain(|job_id, _| {
            self.jobs
                .read(job_id)
                .is_some_and(|record| !record.status.is_finished())
        });
        let job = self.jobs.submit(
            NewJob {
                job_id: JobId::new(),
                kind: JobKind::Task,
                module_id: module_id.to_owned(),
                resource_id: None,
                origin: Some(origin.clone()),
                grants: grant_ids,
                admission: Admission::Bounded,
            },
            run.control.clone(),
            work,
        )?;
        self.tasks.insert(job.job_id.clone(), run);
        Ok(json!({"job_id": job.job_id, "status": job.status}))
    }

    /// A task's result as its job should record it. A success that arrives after the task was asked
    /// to stop is `cancelled`; otherwise every artifact it published is recorded in the catalog
    /// before the job reads succeeded, and a recording that fails fails the job. A failed or
    /// cancelled task records nothing, so what it wrote stays an unreferenced file.
    pub(super) fn task_result(
        &mut self,
        service: &mut EditorService,
        job_id: &JobId,
        result: Result<Value, Error>,
    ) -> Result<Value, Error> {
        let Some(run) = self.tasks.remove(job_id) else {
            return result;
        };
        let running = self
            .jobs
            .read(job_id)
            .is_some_and(|record| record.status == JobStatus::Running);
        if !running {
            return result;
        }
        let value = match result {
            Ok(_) if run.control.is_cancelled() => return Err(run.control.cancelled_error()),
            other => other?,
        };
        for (record, prepared) in run.outcome.take_published() {
            service.register_artifact(record, prepared, true)?;
        }
        Ok(value)
    }

    /// Forget a task that will never report back.
    pub(super) fn forget_task(&mut self, job_id: &JobId) {
        self.tasks.remove(job_id);
    }
}
