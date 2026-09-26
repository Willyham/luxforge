//! Permissions, the `module.permission.*` methods: granting one exact scope with permission
//! authority, recording a denial, revoking a grant and cancelling the jobs that run under it. See
//! `docs/design/module-capabilities.md#capability-and-consent-contract`.
use super::{
    CapabilityHost, announce_once, asset_exists, encode, profile_origin, registered,
    resources::download_scope,
};
use crate::{
    ClientAuthority, EditorService, Error, JobId, ModuleDescriptor, ModuleRegistry,
    api::params::host_params,
    capabilities::{
        descriptor::{CapabilityDescriptor, CapabilityKind},
        grants::{GrantKind, GrantScope, MAX_REASON, NewGrant},
        jobs::{Origin, PERMISSION_REVOKED},
        settings::WriteOutcome,
    },
};
use serde_json::{Value, json};

host_params! {
    /// `module.permission.list`.
    pub(crate) struct PermissionList {
        module_id: Option<String> = "one module's grants and denials; default all",
    }
}

host_params! {
    pub(crate) struct GrantParams {
        module_id: String,
        capability: String,
        scope: Value,
        mutation: MutationRequest,
    }
}

host_params! {
    pub(crate) struct DenyParams {
        module_id: String,
        capability: String,
        scope: Value,
        mutation: MutationRequest,
    }
}

host_params! {
    pub(crate) struct RevokeParams {
        grant_id: String,
        mutation: MutationRequest,
        reason: Option<String> = "1..256 characters; default revoked",
    }
}

impl CapabilityHost {
    /// `module.permission.grant`: only a client with permission authority, only for a scope the
    /// module can use now. The grant records the envelope's actor and request identity; a retry of
    /// the request is answered by the owner's request table before it reaches here.
    pub(crate) fn grant(
        &mut self,
        service: &EditorService,
        authority: ClientAuthority,
        request: GrantParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        if authority != ClientAuthority::Permissions {
            return Err(Error::forbidden(
                "granting a permission needs permission authority",
            ));
        }
        request.mutation.validate()?;
        let registry = service.registry();
        let descriptor = registered(registry, &request.module_id)?;
        let capability = declared_capability(descriptor, &request.capability)?;
        let scope = GrantScope::parse(GrantKind::of(&capability.kind), &request.scope)?;
        self.check_usable(service, descriptor, capability, &scope)?;
        let outcome = self.grants()?.grant(NewGrant {
            module_id: &descriptor.id,
            capability: &capability.id,
            scope,
            actor: &request.mutation.actor,
            request_id: &request.mutation.request_id,
        })?;
        if outcome.outcome == WriteOutcome::Committed {
            announce_once(announce, origin);
        }
        encode(outcome)
    }

    /// Whether the module can use `scope` now: the resource version it declares from its pinned
    /// origin, or a profile of the capability's adapter whose endpoint has that origin, the
    /// capability's data class and an asset of this catalog.
    fn check_usable(
        &self,
        service: &EditorService,
        descriptor: &ModuleDescriptor,
        capability: &CapabilityDescriptor,
        scope: &GrantScope,
    ) -> Result<(), Error> {
        match (&capability.kind, scope) {
            (CapabilityKind::DownloadArtifact { resource }, GrantScope::Download(scope)) => {
                let declared = descriptor.resource(resource).ok_or_else(|| {
                    Error::internal(format!(
                        "capability {} names an undeclared resource",
                        capability.id
                    ))
                })?;
                if *scope != download_scope(declared)? {
                    return Err(Error::validation(format!(
                        "the scope must name resource {resource} version {} from the origin of its pinned URL",
                        declared.version
                    )));
                }
            }
            (CapabilityKind::RemoteImageRequest { adapter, data }, GrantScope::Remote(scope)) => {
                if scope.adapter != *adapter || scope.data != *data {
                    return Err(Error::validation(format!(
                        "capability {} sends {} through adapter {adapter}",
                        capability.id,
                        data.name()
                    )));
                }
                let read = self.settings()?.read(descriptor, self.secrets())?;
                let profile = read.profile(&scope.profile_id).ok_or_else(|| {
                    Error::validation(format!("unknown profile {}", scope.profile_id))
                })?;
                if profile.adapter != *adapter {
                    return Err(Error::validation(format!(
                        "profile {} uses adapter {}, not {adapter}",
                        profile.id, profile.adapter
                    )));
                }
                let origin = profile_origin(descriptor, &profile.fields).ok_or_else(|| {
                    Error::validation(format!("profile {} has no valid endpoint", profile.id))
                })?;
                if origin != scope.origin {
                    return Err(Error::validation(format!(
                        "profile {} sends to {origin}, not {}",
                        profile.id, scope.origin
                    )));
                }
                asset_exists(service, &scope.asset_id)?;
            }
            _ => {
                return Err(Error::internal(
                    "a scope was parsed as another capability kind",
                ));
            }
        }
        Ok(())
    }

    /// `module.permission.deny`: record a "Don't allow" for one exact scope, with the envelope's
    /// actor. Any client may.
    pub(crate) fn deny(
        &mut self,
        registry: &ModuleRegistry,
        request: DenyParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        request.mutation.validate()?;
        let descriptor = registered(registry, &request.module_id)?;
        let capability = declared_capability(descriptor, &request.capability)?;
        let scope = GrantScope::parse(GrantKind::of(&capability.kind), &request.scope)?;
        let denial = self.grants()?.deny(
            &descriptor.id,
            &capability.id,
            scope,
            &request.mutation.actor,
        )?;
        announce_once(announce, origin);
        Ok(json!({"denial": denial}))
    }

    /// `module.permission.revoke`: mark the grant revoked and cancel the jobs running under it.
    /// Any client may. Recipes, history and accepted artifacts are never touched.
    pub(crate) fn revoke(
        &mut self,
        request: RevokeParams,
        origin: &Origin,
        announce: &mut Vec<Origin>,
    ) -> Result<Value, Error> {
        request.mutation.validate()?;
        let reason = request.reason.unwrap_or_else(|| "revoked".to_owned());
        if reason.trim().is_empty() || reason.chars().count() > MAX_REASON {
            return Err(Error::validation(format!(
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
    pub(crate) fn list_permissions(
        &self,
        registry: &ModuleRegistry,
        request: PermissionList,
    ) -> Result<Value, Error> {
        if let Some(module_id) = &request.module_id {
            registered(registry, module_id)?;
        }
        encode(self.grants()?.list(request.module_id.as_deref())?)
    }

    /// Cancel the live jobs running under this grant, as `permission revoked`.
    pub(super) fn cancel_dependents_of(
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
}

fn declared_capability<'a>(
    descriptor: &'a ModuleDescriptor,
    id: &str,
) -> Result<&'a CapabilityDescriptor, Error> {
    descriptor.capability(id).ok_or_else(|| {
        Error::validation(format!(
            "module {} declares no capability {id}",
            descriptor.id
        ))
    })
}
