//! `module.setup.install`: everything one tool still needs, installed under one answer. The host
//! works out which of the setup's resources are missing, asks for every grant they need at once,
//! checks this host can run them and that the transfer lane has room for all of them, and only
//! then queues one install per missing resource, in declared order, each the job
//! `module.resource.install` would queue. See `docs/design/module-capabilities.md#setups`.
use super::{CapabilityHost, registered};
use crate::{
    Error, ModuleRegistry,
    api::{Origin, params::host_params},
    capabilities::{
        consent::{SetupGrant, download_disclosure, setup_consent_required},
        descriptor::{ResourceDescriptor, SetupDescriptor},
        grants::Grant,
        setups::{self, HostFacts, PartDisclosure, SetupDisclosure, SetupRow, setup_minimum},
    },
    jobs::{JobKind, JobRecord, Jobs, Lane},
};
use serde_json::{Value, json};
use std::sync::Arc;

/// The method.
pub const INSTALL_SETUP: &str = "module.setup.install";

host_params! {
    pub(crate) struct SetupInstallParams {
        module_id: String = name(),
        setup: String = name(),
        mutation: MutationRequest,
    }
}

/// One resource of the setup that is not installed, and what installing it needs.
struct Missing<'a> {
    resource: &'a ResourceDescriptor,
    /// The install already queued or running, which the answer joins.
    live: Option<JobRecord>,
    /// The live grant of its download, when there is one.
    grant: Option<Grant>,
}

impl CapabilityHost {
    /// What `module.status` says of the module's setups, from the resource rows it read.
    pub(super) fn setup_rows(
        &self,
        jobs: &Jobs,
        descriptor: &crate::ModuleDescriptor,
        resources: &[crate::capabilities::resources::ResourceRow],
    ) -> Vec<SetupRow> {
        setups::rows(
            descriptor,
            resources,
            &jobs.of_module(&descriptor.id),
            &self.host_facts(),
        )
    }

    fn host_facts(&self) -> HostFacts {
        HostFacts {
            machine_memory_bytes: self.config.machine_memory_bytes,
            backends: self
                .config
                .inference
                .as_ref()
                .map(|runtime| runtime.backends()),
        }
    }

    /// `module.setup.install`. In order, before anything is queued: consent for every missing
    /// download (`consent-required` listing each scope); this host running each missing part's
    /// model and meeting the setup's memory minimum (`not-ready` naming the part or the
    /// minimum); the storage quota and the transfer lane's room for every install it would queue
    /// (`resource-limit`). Then one install per missing resource, in declared order. An install
    /// already queued or running is joined, not repeated; an installed resource is skipped.
    pub(crate) fn setup_install(
        &mut self,
        jobs: &mut Jobs,
        registry: &Arc<ModuleRegistry>,
        request: SetupInstallParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        let descriptor = registered(registry, &request.module_id)?;
        let setup = descriptor.setup(&request.setup).ok_or_else(|| {
            Error::validation(format!(
                "module {} declares no setup {}",
                descriptor.id, request.setup
            ))
        })?;
        let store = self.resources()?.clone();
        let mut missing = Vec::new();
        for id in setup.resources() {
            let resource = descriptor
                .resource(id)
                .ok_or_else(|| Error::internal(format!("setup {} names {id}", setup.id)))?;
            if store.installed(&descriptor.id, resource).is_some() {
                continue;
            }
            missing.push(Missing {
                resource,
                live: jobs.live(JobKind::Install, &descriptor.id, Some(id)),
                grant: None,
            });
        }

        // Consent first, on any machine, for every download the setup would start.
        let mut needed = Vec::new();
        for entry in missing.iter_mut().filter(|entry| entry.live.is_none()) {
            let (capability, scope) = Self::download_capability(descriptor, entry.resource)?;
            let (grant, denied) = self
                .grants()?
                .consent(&descriptor.id, &capability.id, &scope)?;
            match grant {
                Some(grant) => entry.grant = Some(grant),
                None => needed.push(SetupGrant {
                    capability: capability.id.clone(),
                    kind: scope.kind(),
                    scope,
                    denied,
                }),
            }
        }
        if !needed.is_empty() {
            let disclosure = self.setup_disclosure(descriptor, setup, &missing)?;
            return Err(setup_consent_required(
                descriptor, &setup.id, needed, disclosure,
            ));
        }

        // What this host cannot run is refused by name, never attempted.
        let host = self.host_facts();
        for part in &setup.parts {
            let unavailable = missing.iter().find(|entry| {
                part.resources.contains(&entry.resource.id)
                    && !host.runs(descriptor, &entry.resource.id)
            });
            if let Some(entry) = unavailable {
                return Err(Error::not_ready(format!(
                    "{}'s part {} cannot run on this computer: its inference runtime offers none of the backends {} runs on",
                    setup.title, part.name, entry.resource.title
                ))
                .with_data(json!({
                    "setup": setup.id,
                    "requirements": [{"kind": "runtime", "id": entry.resource.id,
                                      "part": part.name, "state": "unavailable"}],
                })));
            }
        }
        if !missing.is_empty()
            && let (Some(minimum), Some(memory)) = (
                setup_minimum(descriptor, setup),
                self.config.machine_memory_bytes,
            )
            && memory < minimum
        {
            return Err(Error::not_ready(format!(
                "{} needs a computer with at least {} of memory; this one has {}",
                setup.title,
                super::resources::gib(minimum),
                super::resources::gib(memory)
            ))
            .with_data(json!({
                "setup": setup.id,
                "requirements": [{"kind": "memory", "id": setup.id,
                                  "required_bytes": minimum, "machine_bytes": memory}],
            })));
        }

        // Room for every install it queues, so a setup never starts half its downloads.
        let queued: Vec<&Missing<'_>> = missing
            .iter()
            .filter(|entry| entry.live.is_none())
            .collect();
        let bytes = queued.iter().fold(0u64, |total, entry| {
            total.saturating_add(entry.resource.total_bytes())
        });
        let used = store.used_bytes();
        if used.saturating_add(bytes) > self.config.resource_quota_bytes {
            return Err(Error::resource_limit(format!(
                "setting up {} ({bytes} bytes) would exceed the resource quota: {used} of {} bytes are in use",
                setup.title, self.config.resource_quota_bytes
            )));
        }
        let room = jobs.lane_room(Lane::Transfer);
        if queued.len() > room {
            return Err(Error::resource_limit(format!(
                "setting up {} queues {} installs and the transfer lane has room for {room}",
                setup.title,
                queued.len()
            ))
            .with_data(json!({"setup": setup.id, "installs": queued.len(), "room": room})));
        }

        let mut answered = Vec::with_capacity(missing.len());
        let mut started: Vec<crate::JobId> = Vec::new();
        for entry in &missing {
            let record = match (&entry.live, &entry.grant) {
                (Some(live), _) => live.clone(),
                (None, Some(grant)) => {
                    let queued = self.queue_install(
                        jobs,
                        registry,
                        descriptor,
                        entry.resource,
                        grant.clone(),
                        &request.mutation.actor,
                        origin,
                    );
                    match queued {
                        Ok(record) => {
                            started.push(record.job_id.clone());
                            record
                        }
                        Err(error) => {
                            // Nothing of a setup whose queueing failed stays queued.
                            for job_id in &started {
                                self.cancel_job(
                                    jobs,
                                    job_id,
                                    crate::jobs::CANCELLED,
                                    Some(origin),
                                    announce,
                                );
                            }
                            return Err(error);
                        }
                    }
                }
                (None, None) => {
                    return Err(Error::internal(format!(
                        "{} has no grant after consent was checked",
                        entry.resource.id
                    )));
                }
            };
            answered.push(json!({
                "resource_id": entry.resource.id,
                "job_id": record.job_id,
                "status": record.status,
            }));
        }
        Ok(json!({
            "module_id": descriptor.id,
            "setup": setup.id,
            "jobs": answered,
        }))
    }

    /// The setup's disclosure: its introduction and parts, and the facts of its missing
    /// downloads summed.
    fn setup_disclosure(
        &self,
        descriptor: &crate::ModuleDescriptor,
        setup: &SetupDescriptor,
        missing: &[Missing<'_>],
    ) -> Result<SetupDisclosure, Error> {
        let store = self.resources()?;
        let is_missing = |id: &str| missing.iter().any(|entry| entry.resource.id == id);
        let mut downloads = Vec::with_capacity(missing.len());
        let (mut bytes, mut origins, mut licenses, mut restrictions) =
            (0u64, Vec::new(), Vec::new(), Vec::new());
        let push = |list: &mut Vec<String>, value: &str| {
            if !list.iter().any(|known| known == value) {
                list.push(value.to_owned());
            }
        };
        for entry in missing {
            let resource = entry.resource;
            let (capability, _) = Self::download_capability(descriptor, resource)?;
            let disclosure = download_disclosure(
                descriptor,
                capability,
                resource,
                &store.version_dir(&descriptor.id, resource),
            );
            bytes = bytes.saturating_add(resource.total_bytes());
            for origin in disclosure
                .destination
                .iter()
                .filter_map(|url| url::Url::parse(url).ok())
                .map(|url| url.origin().ascii_serialization())
                .chain(disclosure.further_origins.iter().cloned())
            {
                push(&mut origins, &origin);
            }
            push(&mut licenses, &resource.license);
            if let Some(restriction) = &resource.use_restriction {
                push(&mut restrictions, restriction);
            }
            downloads.push(disclosure);
        }
        let minimum = setup_minimum(descriptor, setup);
        let host = self.host_facts();
        Ok(SetupDisclosure {
            module: descriptor.title.clone(),
            setup: setup.id.clone(),
            title: setup.title.clone(),
            introduction: setup.introduction.clone(),
            parts: setup
                .parts
                .iter()
                .map(|part| PartDisclosure {
                    name: part.name.clone(),
                    purpose: part.purpose.clone(),
                    bytes: part
                        .resources
                        .iter()
                        .filter_map(|id| descriptor.resource(id))
                        .map(ResourceDescriptor::total_bytes)
                        .sum(),
                    installed: !part.resources.iter().any(|id| is_missing(id)),
                    resources: part.resources.clone(),
                })
                .collect(),
            bytes,
            origins,
            licenses,
            use_restrictions: restrictions,
            minimum_memory: minimum,
            machine_memory_bytes: minimum.and(host.machine_memory_bytes),
            fits: minimum
                .zip(host.machine_memory_bytes)
                .map(|(minimum, memory)| memory >= minimum),
            downloads,
        })
    }
}
