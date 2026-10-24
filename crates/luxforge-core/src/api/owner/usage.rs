//! `usage.read`, `usage.prompt.claim` and `usage.set`: the catalog owner's side of the process-wide
//! usage statistics consent ([`crate::usage::consent`]). Each is one small file read or one locked
//! write of `preferences.json`; nothing here touches a catalog, a photograph or the network.
use super::{Call, Owner};
use crate::{
    Error,
    api::{
        announce_once,
        methods::value,
        params::{NoParams, host_params},
    },
    usage::DISCLOSURE_VERSION,
};
use serde_json::{Value, json};

host_params! {
    pub(in crate::api) struct UsageClaim {
        mutation: MutationRequest,
    }
}

host_params! {
    pub(in crate::api) struct UsageSet {
        enabled: bool = boolean().notes("true opts in, which needs permission authority and disclosure_version; false opts out, which any client may"),
        disclosure_version: Option<u32> = integer(i64::from(DISCLOSURE_VERSION), i64::from(DISCLOSURE_VERSION)).notes("the disclosure the person agreed to, which must be the current one usage.read answers; required with true and refused with false"),
        mutation: MutationRequest,
    }
}

/// `usage.read`.
pub(in crate::api) fn read(owner: &mut Owner, _: &Call<'_>, _: NoParams) -> Result<Value, Error> {
    value(owner.host.usage_consent().read(&owner.host.preferences))
}

/// `usage.prompt.claim`: only the host's own client with permission authority may claim the one
/// startup notice, which the method table declares and the catalog owner checks; a claim that wrote
/// the record is announced.
pub(in crate::api) fn claim(
    owner: &mut Owner,
    call: &Call<'_>,
    _: UsageClaim,
) -> Result<Value, Error> {
    let usage = owner.host.usage_consent();
    let show_prompt = usage.claim_published(&owner.host.preferences)?;
    if show_prompt.value {
        announce_once(&mut owner.announced, &call.origin);
    }
    let show_prompt = show_prompt.finish()?;
    Ok(json!({
        "show_prompt": show_prompt,
        "state": usage.read(&owner.host.preferences),
    }))
}

/// `usage.set`: a change to the stored record is announced, so an open Privacy row reads it again.
pub(in crate::api) fn set(
    owner: &mut Owner,
    call: &Call<'_>,
    params: UsageSet,
) -> Result<Value, Error> {
    let authority = owner.authority(call.client);
    let outcome = owner.host.usage_consent().set_published(
        &owner.host.preferences,
        authority,
        params.enabled,
        params.disclosure_version,
        &params.mutation.request_id,
    )?;
    if outcome.value.changed {
        announce_once(&mut owner.announced, &call.origin);
    }
    let outcome = outcome.finish()?;
    Ok(json!({
        "outcome": if outcome.changed { "applied" } else { "no-op" },
        "state": outcome.state,
    }))
}

/// A retried `usage.*` request the owner's request table answered with its first answer: never an
/// instruction to show the notice again, and the state as it is now rather than as it was, so a
/// stale enable's answer cannot claim collection a later opt-out ended. Its handler does not run.
pub(in crate::api) fn deduplicated(owner: &Owner, method: &str, answer: &mut Value) {
    if !method.starts_with("usage.") {
        return;
    }
    if let Value::Object(fields) = answer {
        if fields.contains_key("show_prompt") {
            fields.insert("show_prompt".into(), Value::Bool(false));
        }
        if fields.contains_key("state") {
            fields.insert(
                "state".into(),
                json!(owner.host.usage_consent().read(&owner.host.preferences)),
            );
        }
    }
}
