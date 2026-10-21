//! Managed resources, the `module.resource.*` methods: each declared resource's state, and an
//! install or removal queued on the transfer lane after consent, the quota and the lane bound are
//! checked. The transfer itself is `capabilities::resources`. See
//! `docs/design/module-capabilities.md#lifecycle-jobs-and-resources`.
use super::{CapabilityHost, ModuleParams, registered};
use crate::{
    Error, JobId, ModuleDescriptor, ModuleRegistry,
    api::{Origin, params::host_params},
    capabilities::{
        consent::{consent_required, download_disclosure},
        descriptor::{CapabilityKind, ResourceDescriptor},
        endpoint::{EndpointClass, parse_endpoint},
        grants::{DownloadScope, GrantScope},
        resources::{
            self as transfer, InstallJob, InstallSource, Requirements, ResourceFileRow,
            ResourceRow, ResourceState,
        },
    },
    jobs::{JobControl, JobKind, JobRecord, JobStatus, Jobs, NewJob, Work},
};
use serde_json::{Value, json};
use std::sync::Arc;

host_params! {
    /// `module.resource.remove`.
    pub(crate) struct ResourceParams {
        module_id: String = name(),
        resource_id: String = name(),
        mutation: MutationRequest,
    }
}

host_params! {
    pub(crate) struct InstallParams {
        module_id: String = name(),
        resource_id: String = name(),
        mutation: MutationRequest,
        source: Option<InstallSource> = json("{kind: download}, the default and only source: the declared URL, under the download-artifact grant"),
    }
}

/// The `download-artifact` scope of a declared resource: its identity, version and the origin of
/// its pinned URL.
pub(super) fn download_scope(resource: &ResourceDescriptor) -> Result<DownloadScope, Error> {
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

impl CapabilityHost {
    /// Every declared resource of a module with its state: installed by its marker, installing or
    /// failed by its jobs, otherwise not installed.
    pub(super) fn resource_rows(
        &self,
        jobs: &Jobs,
        descriptor: &ModuleDescriptor,
    ) -> Vec<ResourceRow> {
        descriptor
            .resources
            .iter()
            .map(|resource| {
                let mut row = ResourceRow {
                    id: resource.id.clone(),
                    title: resource.title.clone(),
                    version: resource.version.clone(),
                    bytes: resource.total_bytes(),
                    sha256: resource.sha256.clone(),
                    license: resource.license.clone(),
                    provenance: resource.provenance.clone(),
                    url: resource.url.clone(),
                    files: resource
                        .pinned_files()
                        .map(|file| ResourceFileRow {
                            name: file.name.to_owned(),
                            bytes: file.bytes,
                        })
                        .collect(),
                    tradeoff: resource.tradeoff.clone(),
                    use_restriction: resource.use_restriction.clone(),
                    requirements: Requirements::of(descriptor, resource),
                    fits: self.fits(resource),
                    state: ResourceState::NotInstalled,
                    staged_bytes: None,
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
                    jobs.live(JobKind::Install, &descriptor.id, Some(&resource.id))
                {
                    row.state = ResourceState::Installing;
                    row.job_id = Some(job.job_id);
                } else if let Some(job) = jobs
                    .last_finished(JobKind::Install, &descriptor.id, Some(&resource.id))
                    .filter(|job| job.status == JobStatus::Failed)
                {
                    row.state = ResourceState::Failed;
                    row.job_id = Some(job.job_id);
                    row.error = job.error;
                }
                if row.state != ResourceState::Installed {
                    row.staged_bytes = self
                        .resources
                        .as_ref()
                        .and_then(|store| store.staged_bytes(&descriptor.id, resource));
                }
                row
            })
            .collect()
    }

    /// `module.resource.list`: the declared resources and the storage they share.
    pub(crate) fn resource_list(
        &self,
        jobs: &Jobs,
        registry: &ModuleRegistry,
        request: ModuleParams,
    ) -> Result<Value, Error> {
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
            "resources": self.resource_rows(jobs, descriptor),
            "storage": storage,
            "machine": self.machine(),
        }))
    }

    /// What `module.resource.list` and `module.status` say of this machine: its memory, when the
    /// host can tell.
    pub(super) fn machine(&self) -> Value {
        json!({"memory_bytes": self.config.machine_memory_bytes})
    }

    /// The inference runtime, for a resource a `local-runtime` capability holds: what an install
    /// asks to prepare the model and a removal asks to forget it. `None` for any other resource or
    /// with no runtime configured.
    fn model_runtime(
        &self,
        descriptor: &ModuleDescriptor,
        resource_id: &str,
    ) -> Option<Arc<dyn crate::inference::InferenceRuntime>> {
        super::runtime::runtime_capability(descriptor, resource_id)
            .ok()
            .and(self.config.inference.clone())
    }

    /// Whether this machine meets `resource`'s memory minimum: `None` when it declares none or the
    /// host cannot tell.
    fn fits(&self, resource: &ResourceDescriptor) -> Option<bool> {
        let minimum = resource.min_memory_bytes?;
        let memory = self.config.machine_memory_bytes?;
        Some(memory >= minimum)
    }

    /// Refuse, by name, an install on a machine with less memory than the resource requires.
    fn check_machine(&self, resource: &ResourceDescriptor) -> Result<(), Error> {
        let (Some(minimum), Some(memory)) =
            (resource.min_memory_bytes, self.config.machine_memory_bytes)
        else {
            return Ok(());
        };
        if memory >= minimum {
            return Ok(());
        }
        Err(Error::resource_limit(format!(
            "{} needs a machine with at least {} of memory; this machine has {}",
            resource.title,
            gib(minimum),
            gib(memory)
        ))
        .with_data(json!({
            "requirements": [{
                "kind": "memory",
                "id": resource.id,
                "required_bytes": minimum,
                "machine_bytes": memory,
            }]
        })))
    }

    /// `module.resource.install`: from the pinned URL under a `download-artifact` grant. Consent,
    /// the quota and the lane bound are checked before anything is queued; a second request joins
    /// the queued or running install, and an installed resource answers at once.
    pub(crate) fn install(
        &mut self,
        jobs: &mut Jobs,
        registry: &Arc<ModuleRegistry>,
        request: InstallParams,
        origin: &Origin,
    ) -> Result<Value, Error> {
        let descriptor = registered(registry, &request.module_id)?;
        let resource = declared_resource(descriptor, &request.resource_id)?;
        let store = self.resources()?.clone();
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
        if let Some(job) = jobs.live(JobKind::Install, &descriptor.id, Some(&resource.id)) {
            return Ok(answer(ResourceState::Installing, Some(&job)));
        }
        let InstallSource::Download {} = request.source.unwrap_or(InstallSource::Download {});
        // A machine below the minimum is refused before any consent is asked: nothing it could
        // allow would let the model run.
        self.check_machine(resource)?;
        let capability = descriptor
            .capabilities
            .iter()
            .find(|capability| {
                matches!(&capability.kind, CapabilityKind::DownloadArtifact { resource: id, .. } if *id == resource.id)
            })
            .ok_or_else(|| {
                Error::validation(format!(
                    "module {} declares no download-artifact capability for resource {}",
                    descriptor.id, resource.id
                ))
            })?;
        let scope = GrantScope::Download(download_scope(resource)?);
        let (grant, denied) = self
            .grants()?
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
        transfer::check_quota(&store, resource, self.config.resource_quota_bytes)?;
        let job_id = JobId::new();
        let control = JobControl::new();
        let permission = super::super::permission_watch::PermissionGuard::new(
            self.grants()?.clone(),
            self.settings()?.clone(),
            descriptor.clone(),
            vec![grant.clone()],
            control.clone(),
        );
        self.permissions
            .register(job_id.clone(), permission.clone())?;
        let job = InstallJob {
            store: store.clone(),
            module_id: descriptor.id.clone(),
            resource: resource.clone(),
            transport: self.transport.clone(),
            registry: registry.clone(),
            quota: self.config.resource_quota_bytes,
            actor: request.mutation.actor.clone(),
            control: control.clone(),
            permission: Some(permission),
            runtime: self.model_runtime(descriptor, &resource.id).map(|runtime| {
                let backends = match super::runtime::runtime_capability(descriptor, &resource.id)
                    .map(|capability| &capability.kind)
                {
                    Ok(CapabilityKind::LocalRuntime { backends, .. }) => backends.clone(),
                    _ => Vec::new(),
                };
                (runtime, backends)
            }),
        };
        let record = jobs
            .submit(
                NewJob {
                    job_id: job_id.clone(),
                    kind: JobKind::Install,
                    module_id: Some(descriptor.id.clone()),
                    resource_id: Some(resource.id.clone()),
                    asset_id: None,
                    origin: Some(origin.clone()),
                    grants: vec![grant.grant_id],
                    activity: None,
                },
                control,
                Box::new(move || transfer::install(job)),
            )
            .inspect_err(|_| self.permissions.remove(&job_id))?;
        Ok(answer(ResourceState::Installing, Some(&record)))
    }

    /// `module.resource.remove`: queue the removal of the installed version, joining one already
    /// queued. A model a `local-runtime` capability holds of it, or opens with it, is released
    /// first, and its load cancelled, so nothing keeps a removed model loaded.
    pub(crate) fn remove(
        &mut self,
        jobs: &mut Jobs,
        registry: &Arc<ModuleRegistry>,
        request: ResourceParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let descriptor = registered(registry, &request.module_id)?;
        let resource = declared_resource(descriptor, &request.resource_id)?;
        let store = self.resources()?.clone();
        let held: Vec<String> = descriptor
            .capabilities
            .iter()
            .filter_map(|capability| match &capability.kind {
                CapabilityKind::LocalRuntime {
                    resource: held,
                    with,
                    ..
                } if *held == resource.id || with.contains(&resource.id) => Some(held.clone()),
                _ => None,
            })
            .collect();
        if !held.is_empty() {
            self.unload_models(jobs, &descriptor.id, &held, announce);
        }
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
            .resource_rows(jobs, descriptor)
            .into_iter()
            .find(|row| row.id == resource.id)
            .map_or(ResourceState::NotInstalled, |row| row.state);
        if let Some(job) = jobs.live(JobKind::Remove, &descriptor.id, Some(&resource.id)) {
            return Ok(answer(Some(&job), state));
        }
        let control = JobControl::new();
        let work: Work = {
            let control = control.clone();
            let module_id = descriptor.id.clone();
            let resource = resource.clone();
            let runtime = self.model_runtime(descriptor, &resource.id);
            Box::new(move || {
                transfer::remove(&store, &module_id, &resource, &control, runtime.as_deref())
            })
        };
        let record = jobs.submit(
            NewJob {
                job_id: JobId::new(),
                kind: JobKind::Remove,
                module_id: Some(descriptor.id.clone()),
                resource_id: Some(resource.id.clone()),
                asset_id: None,
                origin: Some(origin.clone()),
                grants: Vec::new(),
                activity: None,
            },
            control,
            work,
        )?;
        Ok(answer(Some(&record), state))
    }
}

fn declared_resource<'a>(
    descriptor: &'a ModuleDescriptor,
    id: &str,
) -> Result<&'a ResourceDescriptor, Error> {
    descriptor.resource(id).ok_or_else(|| {
        Error::validation(format!(
            "module {} declares no resource {id}",
            descriptor.id
        ))
    })
}

/// A byte count in gibibytes as a person reads memory: whole, or with one decimal under ten.
pub(crate) fn gib(bytes: u64) -> String {
    let value = bytes as f64 / (1u64 << 30) as f64;
    if value < 10.0 && value.fract() != 0.0 {
        format!("{value:.1} GB")
    } else {
        format!("{value:.0} GB")
    }
}
