//! The owner handlers of the `module.*` methods and the generated `task.*` methods: each passes its
//! parsed parameters to one capability host operation, with the registry, the caller's authority
//! where it matters and the call's origin, under which the host announces what the call changed.
use super::{Call, Owner};
use crate::{ClientAuthority, Error, ErrorKind, capabilities::host::TASK_PREFIX};
use serde_json::Value;

pub(in crate::api) use crate::capabilities::host::{
    ClearSecretParams, CreateProfileParams, DenyParams, GrantParams, InstallParams,
    JobCancelParams, JobParams, ModuleChange, ModuleParams, PermissionList, RemoveProfileParams,
    ResetParams, ResourceParams, RevokeParams, SetParams, SetSecretParams,
};

pub(in crate::api) fn settings_read(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ModuleParams,
) -> Result<Value, Error> {
    owner.host.read(owner.service.registry(), params)
}

pub(in crate::api) fn settings_set(
    owner: &mut Owner,
    call: &Call<'_>,
    params: SetParams,
) -> Result<Value, Error> {
    owner.host.settings_set(
        owner.service.registry(),
        params,
        &call.origin,
        &mut owner.announced,
    )
}

pub(in crate::api) fn settings_set_secret(
    owner: &mut Owner,
    call: &Call<'_>,
    params: SetSecretParams,
) -> Result<Value, Error> {
    owner.host.settings_set_secret(
        owner.service.registry(),
        params,
        &call.origin,
        &mut owner.announced,
    )
}

pub(in crate::api) fn settings_clear_secret(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ClearSecretParams,
) -> Result<Value, Error> {
    owner.host.settings_clear_secret(
        owner.service.registry(),
        params,
        &call.origin,
        &mut owner.announced,
    )
}

pub(in crate::api) fn settings_reset(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ResetParams,
) -> Result<Value, Error> {
    owner.host.settings_reset(
        owner.service.registry(),
        params,
        &call.origin,
        &mut owner.announced,
    )
}

pub(in crate::api) fn profile_create(
    owner: &mut Owner,
    call: &Call<'_>,
    params: CreateProfileParams,
) -> Result<Value, Error> {
    owner.host.profile_create(
        owner.service.registry(),
        params,
        &call.origin,
        &mut owner.announced,
    )
}

pub(in crate::api) fn profile_remove(
    owner: &mut Owner,
    call: &Call<'_>,
    params: RemoveProfileParams,
) -> Result<Value, Error> {
    owner.host.profile_remove(
        owner.service.registry(),
        params,
        &call.origin,
        &mut owner.announced,
    )
}

/// A client's authority is part of its session, fixed when it registered.
fn authority(owner: &Owner, call: &Call<'_>) -> ClientAuthority {
    owner
        .sessions
        .get(&call.client)
        .map_or(ClientAuthority::Edit, |session| session.authority)
}

pub(in crate::api) fn permission_grant(
    owner: &mut Owner,
    call: &Call<'_>,
    params: GrantParams,
) -> Result<Value, Error> {
    let authority = authority(owner, call);
    owner.host.grant(
        &owner.service,
        authority,
        params,
        &call.origin,
        &mut owner.announced,
    )
}

pub(in crate::api) fn permission_deny(
    owner: &mut Owner,
    call: &Call<'_>,
    params: DenyParams,
) -> Result<Value, Error> {
    owner.host.deny(
        owner.service.registry(),
        params,
        &call.origin,
        &mut owner.announced,
    )
}

pub(in crate::api) fn permission_revoke(
    owner: &mut Owner,
    call: &Call<'_>,
    params: RevokeParams,
) -> Result<Value, Error> {
    owner
        .host
        .revoke(params, &call.origin, &mut owner.announced)
}

pub(in crate::api) fn permission_list(
    owner: &mut Owner,
    _: &Call<'_>,
    params: PermissionList,
) -> Result<Value, Error> {
    owner
        .host
        .list_permissions(owner.service.registry(), params)
}

pub(in crate::api) fn module_activate(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ModuleChange,
) -> Result<Value, Error> {
    owner
        .host
        .activate(owner.service.registry(), params, &call.origin)
}

pub(in crate::api) fn module_deactivate(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ModuleChange,
) -> Result<Value, Error> {
    owner.host.deactivate_request(
        owner.service.registry(),
        params,
        &call.origin,
        &mut owner.announced,
    )
}

pub(in crate::api) fn module_status(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ModuleParams,
) -> Result<Value, Error> {
    owner.host.status(owner.service.registry(), params)
}

pub(in crate::api) fn resource_list(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ModuleParams,
) -> Result<Value, Error> {
    owner.host.resource_list(owner.service.registry(), params)
}

pub(in crate::api) fn resource_install(
    owner: &mut Owner,
    call: &Call<'_>,
    params: InstallParams,
) -> Result<Value, Error> {
    owner
        .host
        .install(owner.service.registry(), params, &call.origin)
}

pub(in crate::api) fn resource_remove(
    owner: &mut Owner,
    call: &Call<'_>,
    params: ResourceParams,
) -> Result<Value, Error> {
    owner.host.remove(
        owner.service.registry(),
        params,
        &call.origin,
        &mut owner.announced,
    )
}

pub(in crate::api) fn job_read(
    owner: &mut Owner,
    _: &Call<'_>,
    params: JobParams,
) -> Result<Value, Error> {
    owner.host.job_read(params)
}

pub(in crate::api) fn job_cancel(
    owner: &mut Owner,
    call: &Call<'_>,
    params: JobCancelParams,
) -> Result<Value, Error> {
    owner
        .host
        .job_cancel(params, &call.origin, &mut owner.announced)
}

/// A generated `task.<id>` method. The request queues a capability job and announces nothing: the
/// task is announced when it succeeds. A task samples its asset's current entry before it is
/// queued, so an unprepared source or artifact queues that preparation and answers with the job to
/// wait for, as every other evaluating request does.
pub(in crate::api) fn task(owner: &mut Owner, call: &Call<'_>) -> Result<Value, Error> {
    let task_id = call
        .request
        .method
        .strip_prefix(TASK_PREFIX)
        .unwrap_or_default();
    match owner
        .host
        .task(&owner.service, task_id, &call.request.params, &call.origin)
    {
        Err(error) if error.kind == ErrorKind::PreparationRequired => {
            Err(owner.prepare_request(call.client, &call.request.params, &error))
        }
        other => other,
    }
}
