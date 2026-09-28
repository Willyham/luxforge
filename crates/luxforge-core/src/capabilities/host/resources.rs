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
        resources::{self as transfer, InstallJob, InstallSource, ResourceRow, ResourceState},
    },
    jobs::{JobControl, JobKind, JobRecord, JobStatus, Jobs, NewJob, Work},
};
use serde_json::{Value, json};
use std::sync::Arc;

host_params! {
    /// `module.resource.remove`.
    pub(crate) struct ResourceParams {
        module_id: String,
        resource_id: String,
        mutation: MutationRequest,
    }
}

host_params! {
    pub(crate) struct InstallParams {
        module_id: String,
        resource_id: String,
        mutation: MutationRequest,
        source: Option<InstallSource> = "{kind: download}, the default and only source: the declared URL, under the download-artifact grant",
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
        }))
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
        if let Some(job) = jobs.live(JobKind::Install, &descriptor.id, Some(&resource.id)) {
            return Ok(answer(ResourceState::Installing, Some(&job)));
        }
        let InstallSource::Download {} = request.source.unwrap_or(InstallSource::Download {});
        let capability = descriptor
            .capabilities
            .iter()
            .find(|capability| {
                matches!(&capability.kind, CapabilityKind::DownloadArtifact { resource: id } if *id == resource.id)
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
        transfer::check_quota(store, resource, self.config.resource_quota_bytes)?;
        let job_id = JobId::new();
        let control = JobControl::new();
        let job = InstallJob {
            store: store.clone(),
            job_id: job_id.clone(),
            module_id: descriptor.id.clone(),
            resource: resource.clone(),
            transport: self.transport.clone(),
            registry: registry.clone(),
            quota: self.config.resource_quota_bytes,
            actor: request.mutation.actor.clone(),
            control: control.clone(),
        };
        let record = jobs.submit(
            NewJob {
                job_id,
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
        )?;
        Ok(answer(ResourceState::Installing, Some(&record)))
    }

    /// `module.resource.remove`: queue the removal of the installed version, joining one already
    /// queued.
    pub(crate) fn remove(
        &mut self,
        jobs: &mut Jobs,
        registry: &Arc<ModuleRegistry>,
        request: ResourceParams,
        origin: &Origin,
    ) -> Result<Value, Error> {
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
            .resource_rows(jobs, descriptor)
            .into_iter()
            .find(|row| row.id == resource.id)
            .map_or(ResourceState::NotInstalled, |row| row.state);
        if let Some(job) = jobs.live(JobKind::Remove, &descriptor.id, Some(&resource.id)) {
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
            Box::new(move || transfer::remove(&store, &module_id, &resource, &control))
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
