//! The `local-runtime` methods: `module.runtime.load`, which queues a module-lane job that opens a
//! model ahead of its first task, and `module.runtime.unload`, which releases one or all of a
//! module's models at once. The resident set itself is `capabilities::runtime`. See
//! `docs/design/module-capabilities.md#local-runtime`.
use super::{CapabilityHost, Requirement, RequirementKind, registered};
use crate::{
    Availability, Error, JobId, ModuleDescriptor, ModuleRegistry,
    api::{Origin, announce_once, params::host_params},
    capabilities::{
        descriptor::{CapabilityDescriptor, CapabilityKind},
        resources::ResourceState,
        runtime::{LoadRequest, UNLOADED},
    },
    inference::{ModelFile, ModelIdentity, OpenOptions},
    jobs::{JobControl, JobKind, Jobs, NewJob, Work},
};
use serde_json::{Value, json};
use std::sync::Arc;

host_params! {
    /// `module.runtime.load`.
    pub(crate) struct LoadParams {
        module_id: String = name(),
        resource_id: String = name().notes("a resource a local-runtime capability of the module holds"),
    }
}

host_params! {
    /// `module.runtime.unload`.
    pub(crate) struct UnloadParams {
        module_id: String = name(),
        resource_id: Option<String> = name().notes("one resource a local-runtime capability holds; default every model of the module"),
    }
}

/// The `local-runtime` capability of `descriptor` that holds `resource_id`.
pub(super) fn runtime_capability<'a>(
    descriptor: &'a ModuleDescriptor,
    resource_id: &str,
) -> Result<&'a CapabilityDescriptor, Error> {
    descriptor
        .capabilities
        .iter()
        .find(|capability| {
            matches!(&capability.kind, CapabilityKind::LocalRuntime { resource, .. } if resource == resource_id)
        })
        .ok_or_else(|| {
            Error::validation(format!(
                "module {} declares no local-runtime capability for resource {resource_id}",
                descriptor.id
            ))
        })
}

impl CapabilityHost {
    /// The load a `local-runtime` capability names, with its installed resource's file; `None`
    /// with the unmet requirement when the resource is not installed. Stats only.
    pub(super) fn load_request(
        &self,
        jobs: &Jobs,
        descriptor: &ModuleDescriptor,
        capability: &CapabilityDescriptor,
    ) -> Result<Result<LoadRequest, Requirement>, Error> {
        let CapabilityKind::LocalRuntime {
            resource,
            model,
            resident_bytes,
            backends,
            precision,
            with,
        } = &capability.kind
        else {
            return Err(Error::internal(format!(
                "capability {} is not local-runtime",
                capability.id
            )));
        };
        let rows = self.resource_rows(jobs, descriptor);
        // The installed file of one declared resource, or the requirement it leaves unmet.
        let installed = |id: &String| -> Result<Result<ModelFile, Requirement>, Error> {
            let declared = descriptor.resource(id).ok_or_else(|| {
                Error::internal(format!(
                    "capability {} names an undeclared resource",
                    capability.id
                ))
            })?;
            let row = rows.iter().find(|row| &row.id == id);
            Ok(match row {
                Some(row) if row.state == ResourceState::Installed && row.path.is_some() => {
                    Ok(ModelFile::new(
                        ModelIdentity {
                            id: id.clone(),
                            version: declared.version.clone(),
                            sha256: declared.sha256.clone(),
                        },
                        row.path.clone().unwrap_or_default(),
                    ))
                }
                row => Err(Requirement {
                    kind: RequirementKind::Resource,
                    id: id.clone(),
                    state: row
                        .map_or(ResourceState::NotInstalled, |row| row.state)
                        .name()
                        .into(),
                }),
            })
        };
        let file = match installed(resource)? {
            Ok(file) => file,
            Err(missing) => return Ok(Err(missing)),
        };
        let mut companions = Vec::with_capacity(with.len());
        for companion in with {
            match installed(companion)? {
                Ok(file) => companions.push(file),
                Err(missing) => return Ok(Err(missing)),
            }
        }
        Ok(Ok(LoadRequest {
            module_id: descriptor.id.clone(),
            resource_id: resource.clone(),
            capability_id: capability.id.clone(),
            model: *model,
            resident_bytes: *resident_bytes,
            file: file.with_companions(companions),
            options: OpenOptions {
                backends: backends.clone(),
                precision: *precision,
            },
        }))
    }

    /// `module.runtime.load`: answer `loaded` at once for a resident model, join a load already
    /// queued or running, or check the module, its runtime, the installed resource and the
    /// resident budget and queue a module-lane job that opens it. Opens nothing on the owner.
    pub(crate) fn runtime_load(
        &mut self,
        jobs: &mut Jobs,
        registry: &Arc<ModuleRegistry>,
        request: LoadParams,
        origin: &Origin,
    ) -> Result<Value, Error> {
        let descriptor = registered(registry, &request.module_id)?;
        if let Availability::Unavailable { reason } = &descriptor.availability {
            return Err(Error::incompatible(format!(
                "unavailable module {} cannot load a model: {reason}",
                descriptor.id
            )));
        }
        let capability = runtime_capability(descriptor, &request.resource_id)?;
        let answer = |state: &str, job: Option<(&JobId, crate::JobStatus)>| {
            let mut value = json!({
                "module_id": descriptor.id,
                "resource_id": request.resource_id,
                "state": state,
            });
            if let Some((job_id, status)) = job {
                value["job_id"] = json!(job_id);
                value["status"] = json!(status);
            }
            value
        };
        if self.runtime.touch(&descriptor.id, &request.resource_id) {
            return Ok(answer("loaded", None));
        }
        if let Some(job) = jobs.live(JobKind::Load, &descriptor.id, Some(&request.resource_id)) {
            return Ok(answer("loading", Some((&job.job_id, job.status))));
        }
        self.runtime.configured()?;
        let load = match self.load_request(jobs, descriptor, capability)? {
            Ok(load) => load,
            Err(missing) => {
                return Err(Error::not_ready(format!(
                    "model {} is not ready: {} {} is {}",
                    request.resource_id,
                    missing.kind.name(),
                    missing.id,
                    missing.state
                ))
                .with_data(json!({"requirements": [missing]})));
            }
        };
        self.runtime.admits(load.resident_bytes)?;
        let control = JobControl::new();
        let work: Work = {
            let runtime = self.runtime.clone();
            let control = control.clone();
            Box::new(move || {
                control.set_progress(None, "opening the model");
                let loaded = runtime.acquire(&load, control.render_cancel())?;
                drop(loaded);
                control.checkpoint()?;
                Ok(json!({
                    "module_id": load.module_id,
                    "resource_id": load.resource_id,
                    "resident_bytes": load.resident_bytes,
                }))
            })
        };
        let record = jobs.submit(
            NewJob {
                job_id: JobId::new(),
                kind: JobKind::Load,
                module_id: Some(descriptor.id.clone()),
                resource_id: Some(request.resource_id.clone()),
                asset_id: None,
                origin: Some(origin.clone()),
                grants: Vec::new(),
                activity: None,
            },
            control,
            work,
        )?;
        Ok(answer("loading", Some((&record.job_id, record.status))))
    }

    /// `module.runtime.unload`: release `resource_id`'s model, or all of the module's, at once, and
    /// cancel their queued or running loads. A task already using a model keeps it until it ends.
    /// Announced when something was released or cancelled.
    pub(crate) fn runtime_unload(
        &mut self,
        jobs: &mut Jobs,
        registry: &Arc<ModuleRegistry>,
        request: UnloadParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let descriptor = registered(registry, &request.module_id)?;
        let resources: Vec<String> = match &request.resource_id {
            Some(resource_id) => {
                runtime_capability(descriptor, resource_id)?;
                vec![resource_id.clone()]
            }
            None => {
                let held: Vec<String> = descriptor
                    .capabilities
                    .iter()
                    .filter_map(|capability| match &capability.kind {
                        CapabilityKind::LocalRuntime { resource, .. } => Some(resource.clone()),
                        _ => None,
                    })
                    .collect();
                if held.is_empty() {
                    return Err(Error::validation(format!(
                        "module {} declares no local-runtime capability",
                        descriptor.id
                    )));
                }
                held
            }
        };
        let (unloaded, cancelled) = self.unload_models(jobs, &descriptor.id, &resources, announce);
        if !unloaded.is_empty() || !cancelled.is_empty() {
            announce_once(announce, origin);
        }
        Ok(json!({
            "module_id": descriptor.id,
            "unloaded": unloaded,
            "cancelled_jobs": cancelled,
        }))
    }

    /// Release `resources` of `module_id` and cancel their live loads: what an unload does, and a
    /// resource's removal. Answers the resources released and the jobs cancelled.
    pub(super) fn unload_models(
        &mut self,
        jobs: &mut Jobs,
        module_id: &str,
        resources: &[String],
        announce: &mut Vec<Origin>,
    ) -> (Vec<String>, Vec<JobId>) {
        let mut cancelled = Vec::new();
        for resource in resources {
            if let Some(job) = jobs.live(JobKind::Load, module_id, Some(resource)) {
                let origin = jobs.origin(&job.job_id).cloned();
                if self
                    .cancel_job(jobs, &job.job_id, UNLOADED, origin.as_ref(), announce)
                    .is_some()
                {
                    cancelled.push(job.job_id);
                }
            }
        }
        let mut unloaded = Vec::new();
        for resource in resources {
            unloaded.extend(self.runtime.unload(module_id, Some(resource)));
        }
        (unloaded, cancelled)
    }
}
