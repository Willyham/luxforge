//! Managed resources, the `module.resource.*` methods: each declared resource's state, and an
//! install or removal queued on the transfer lane after consent, the quota and the lane bound are
//! checked. The transfer itself is `capabilities::resources`. See
//! `docs/design/module-capabilities.md#lifecycle-jobs-and-resources`.
use super::{CapabilityHost, ModuleParams, RESOURCE_REMOVED, registered};
use crate::{
    Error, JobId, ModuleDescriptor, ModuleRegistry,
    api::params::host_params,
    capabilities::{
        consent::{consent_required, download_disclosure},
        descriptor::{CapabilityKind, ResourceDescriptor},
        grants::{DownloadScope, GrantScope},
        jobs::{Admission, JobControl, JobKind, JobRecord, JobStatus, NewJob, Origin, Work},
        resources::{
            self as transfer, Fetch, InstallJob, InstallSource, ResourceRow, ResourceState,
        },
        transport::{EndpointClass, parse_endpoint},
    },
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
        source: Option<InstallSource> = "{kind: download} (default), which needs the download-artifact grant, or {kind: file, path} to copy a local file, which needs none because only the pinned bytes are accepted",
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
    pub(super) fn resource_rows(&self, descriptor: &ModuleDescriptor) -> Vec<ResourceRow> {
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
    pub(crate) fn resource_list(
        &self,
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
            "resources": self.resource_rows(descriptor),
            "storage": storage,
        }))
    }

    /// `module.resource.install`: from the pinned URL under a `download-artifact` grant, or from a
    /// local file whose bytes must match the pinned hash. Consent, the quota and the lane bound
    /// are checked before anything is queued; a second request joins the queued or running
    /// install, and an installed resource answers at once.
    pub(crate) fn install(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: InstallParams,
        origin: &Origin,
    ) -> Result<Value, Error> {
        request.mutation.validate()?;
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
        let (fetch, grants) = match request.source.unwrap_or_default() {
            InstallSource::Download => {
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
                    return Err(Error::validation(format!(
                        "{} is not a readable file",
                        path.display()
                    )));
                }
                (Fetch::File(path), Vec::new())
            }
        };
        transfer::check_quota(store, resource, self.config.resource_quota_bytes)?;
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
            actor: request.mutation.actor.clone(),
            control: control.clone(),
        };
        let record = self.jobs.submit(
            NewJob {
                job_id,
                kind: JobKind::Install,
                module_id: descriptor.id.clone(),
                resource_id: Some(resource.id.clone()),
                origin: Some(origin.clone()),
                grants,
                admission: Admission::Bounded,
            },
            control,
            Box::new(move || transfer::install(job)),
        )?;
        Ok(answer(ResourceState::Installing, Some(&record)))
    }

    /// `module.resource.remove`: queue the removal of the installed version, joining one already
    /// queued. A module that is active or activating and requires the resource is deactivated first.
    pub(crate) fn remove(
        &mut self,
        registry: &Arc<ModuleRegistry>,
        request: ResourceParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        request.mutation.validate()?;
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
            Box::new(move || transfer::remove(&store, &module_id, &resource, &control))
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
