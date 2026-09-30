//! The method table: every host method is one entry with its declared parameters, notes and
//! handler, and every module action, query and task resolves to a generated method from the same
//! registry, so discovery, event emission and dispatch cannot drift apart.
//!
//! A handler is a service handler, which the editor service answers with the caller's session, or
//! an owner handler, which the catalog owner answers from its own state: its jobs, its event log,
//! the capability host and the activity board. A handler that only forwards to one operation is
//! written inline in its entry. The catalog owner finds a method here, checks its mutation envelope
//! once ([`Envelope::check`]), calls its handler and records the event a change announces, which a
//! mutating service handler reports beside its answer ([`Mutated`]); nothing is routed any other
//! way.
use super::{
    ClientSession, MASK_MODE, MaskOverlayColour, MaskOverlayMode, POINTER_MODE, PROTOCOL,
    owner::{self, Call, Owner},
    params::{self, Envelope, HostParams, NoParams, ParamSchema, host_params, parse},
};
#[cfg(test)]
use crate::ErrorKind;
use crate::{
    ActionRef, AnalysisSelection, ArtifactId, AssetId, ComponentId, DraftId, EditorService,
    EntryId, Error, HistorySelection, MAX_PRESET_BYTES, MAX_PRESET_NAME, MaskId, ModuleRegistry,
    Mutation, MutationOutcome, MutationResult, ParameterDescriptor, PixelSample, PresetId, Zoom,
    capabilities::{
        grants,
        host::{
            self, ClearSecretParams, CreateProfileParams, DenyParams, GrantParams, InstallParams,
            ModuleParams, PermissionList, RemoveProfileParams, ResetParams, ResourceParams,
            RevokeParams, SetParams, SetSecretParams, TASK_PREFIX,
        },
        resources, settings,
    },
    editor::{DEFAULT_ASSET_PAGE, MAX_ASSET_PAGE, MAX_HISTORY_PAGE, MAX_VERSION_NAME, PointPlan},
    jobs::{JOB_CANCEL, JOB_READ},
    path,
    presets::MAX_PRESET_GROUP,
};
use serde::Serialize;
use serde_json::{Map, Value, json};

/// A method the editor service answers with the caller's session that changes nothing the owner
/// announces: a read, or a change to the caller's own session.
pub(super) type ServiceHandler =
    fn(&mut EditorService, &mut ClientSession, &Value) -> Result<Value, Error>;

/// A mutating method the editor service answers with the caller's session. It reports what it
/// changed beside its answer ([`Mutated`]), so nothing reads that back out of the answer.
pub(super) type MutatingHandler =
    fn(&mut EditorService, &mut ClientSession, &Value) -> Result<Mutated, Error>;

/// A method the editor service plans with the caller's session, whose answer may be evaluated
/// after the catalog owner has moved on: [`Planned`].
pub(super) type PlannedHandler =
    fn(&mut EditorService, &mut ClientSession, &Value) -> Result<Planned, Error>;

/// A method the catalog owner answers from its own state.
pub(super) type OwnerHandler = fn(&mut Owner, &Call<'_>) -> Result<Value, Error>;

pub(super) enum Handler {
    Service(ServiceHandler),
    Mutating(MutatingHandler),
    Planned(PlannedHandler),
    Owner(OwnerHandler),
}

/// What a mutating service method changed, as its handler reports it. The catalog owner announces
/// a change from this alone, with the revision named here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Changed {
    /// Nothing: a read, a no-op, or a retry a request log answered, whose first attempt announced
    /// the change.
    Nothing,
    /// Something, and the revision it left when it changed an asset.
    Something { revision: Option<u64> },
}

/// A mutating service method's answer and what it changed.
pub(super) struct Mutated {
    pub value: Value,
    pub changed: Changed,
}

impl Mutated {
    /// A change to an asset's history, whose retry the catalog's request log answers: it changed
    /// something unless it was a no-op or that retry, and it left the asset at its revision.
    fn asset(result: &MutationResult, answer: impl Serialize) -> Result<Self, Error> {
        let changed = if result.outcome == MutationOutcome::NoOp || result.deduplicated {
            Changed::Nothing
        } else {
            Changed::Something {
                revision: Some(result.revision),
            }
        };
        Ok(Self {
            value: value(answer)?,
            changed,
        })
    }

    /// A change to something without a revision, whose retry the owner's request table answers
    /// before the handler runs: it changed something unless its outcome is a no-op.
    fn unrevised(outcome: MutationOutcome, answer: impl Serialize) -> Result<Self, Error> {
        let changed = match outcome {
            MutationOutcome::NoOp => Changed::Nothing,
            _ => Changed::Something { revision: None },
        };
        Ok(Self {
            value: value(answer)?,
            changed,
        })
    }
}

/// What a planned service method answers: a value, or a point sample through a spatial layer,
/// planned on the catalog owner in `O(layers)` and evaluated by whoever holds it. The owner hands
/// the sample to its point worker; every other caller evaluates it where it stands. A planned
/// method carries no mutation envelope, so no request table waits for its answer.
pub(super) enum Planned {
    Value(Value),
    Sample(Box<PointPlan>),
}

impl Planned {
    /// The answer, evaluating a planned sample here.
    pub(super) fn answer(self) -> Result<Value, Error> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Sample(plan) => sample_value((*plan).evaluate()?),
        }
    }
}

pub(super) struct MethodSpec {
    pub name: &'static str,
    /// The parameters, generated with the struct the handler parses.
    pub params: &'static ParamSchema,
    pub notes: &'static str,
    pub handler: Handler,
    /// Who answers a retry of this method. A mutating entry names it after its notes, as
    /// `retries: Catalog` or `retries: Owner`; an entry that names none changes nothing.
    pub retries: Retries,
}

/// Who answers a retried mutation, the same `request_id` with the same input, so that resending a
/// request whose answer was lost never changes anything twice. Each method declares it, and the
/// catalog owner reads nothing else to decide.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Retries {
    /// Nobody: the method changes nothing, so a repeat simply runs again.
    None,
    /// The catalog, from the request log it writes in the same transaction as an asset's change.
    /// The handler runs again and the catalog answers before it plans anything, durably across a
    /// restart.
    Catalog,
    /// The catalog owner's request table: a retry gets the first answer and the handler does not
    /// run. For a change with no request log of its own, and for one whose handler ends the state
    /// its request names, as `draft.commit` ends its draft, so a retry could not reach the
    /// catalog's log.
    Owner,
}

/// The retry route a table entry declares: [`Retries::None`] when it names none.
macro_rules! retries {
    () => {
        Retries::None
    };
    ($retries:ident) => {
        Retries::$retries
    };
}

/// One service method that changes nothing the owner announces. Its handler takes the struct its
/// schema was generated from, parsed from the request by the one parse function, so the two cannot
/// differ: a function, or a handler that only forwards, written inline as
/// `|service, session, params| expression`.
macro_rules! service {
    ($name:expr, $params:ty,
        |$service:pat_param, $session:pat_param, $parsed:pat_param| $body:expr,
        $notes:expr $(,)?) => {
        MethodSpec {
            name: $name,
            params: &<$params as HostParams>::SCHEMA,
            notes: $notes,
            handler: Handler::Service(|service, session, request| {
                let $parsed: $params = parse(request)?;
                let ($service, $session) = (service, session);
                $body
            }),
            retries: Retries::None,
        }
    };
    ($name:expr, $params:ty, $handler:path, $notes:expr $(,)?) => {
        MethodSpec {
            name: $name,
            params: &<$params as HostParams>::SCHEMA,
            notes: $notes,
            handler: Handler::Service(|service, session, request| {
                $handler(service, session, parse::<$params>(request)?)
            }),
            retries: Retries::None,
        }
    };
}

/// One mutating service method, parsed the same way, which reports what it changed and declares who
/// answers its retries.
macro_rules! mutating {
    ($name:expr, $params:ty, $handler:path, $notes:expr, retries: $retries:ident $(,)?) => {
        MethodSpec {
            name: $name,
            params: &<$params as HostParams>::SCHEMA,
            notes: $notes,
            handler: Handler::Mutating(|service, session, request| {
                $handler(service, session, parse::<$params>(request)?)
            }),
            retries: Retries::$retries,
        }
    };
}

/// One planned service method, parsed the same way.
macro_rules! planned {
    ($name:expr, $params:ty, $handler:path, $notes:expr $(,)?) => {
        MethodSpec {
            name: $name,
            params: &<$params as HostParams>::SCHEMA,
            notes: $notes,
            handler: Handler::Planned(|service, session, request| {
                $handler(service, session, parse::<$params>(request)?)
            }),
            retries: Retries::None,
        }
    };
}

/// One owner method, parsed the same way: a function, or a handler that only forwards to one
/// operation, written inline as `|owner, call, params| expression`, with the owner's fields
/// borrowed apart.
macro_rules! owner {
    ($name:expr, $params:ty,
        |$owner:pat_param, $call:pat_param, $parsed:pat_param| $body:expr,
        $notes:expr $(, retries: $retries:ident)? $(,)?) => {
        MethodSpec {
            name: $name,
            params: &<$params as HostParams>::SCHEMA,
            notes: $notes,
            handler: Handler::Owner(|owner, call| {
                let $parsed: $params = parse(&call.request.params)?;
                let ($owner, $call) = (owner, call);
                $body
            }),
            retries: retries!($($retries)?),
        }
    };
    ($name:expr, $params:ty, $handler:path, $notes:expr $(, retries: $retries:ident)? $(,)?) => {
        MethodSpec {
            name: $name,
            params: &<$params as HostParams>::SCHEMA,
            notes: $notes,
            handler: Handler::Owner(|owner, call| {
                $handler(owner, call, parse::<$params>(&call.request.params)?)
            }),
            retries: retries!($($retries)?),
        }
    };
}

pub(super) const METHODS: &[MethodSpec] = &[
    service!(
        "schema.list",
        NoParams,
        |service, _, _| Ok(schemas(service.registry())),
        "protocol identity and every method with its parameters; a generated method lists the source kinds its module applies to as sources when that is not every kind, and a parameter another module's control variant supersedes on a kind's global target lists superseded: [{source, by}], the field that is its one path there"
    ),
    owner!(
        "catalog.import",
        owner::Import,
        owner::catalog_import,
        "queues bounded source preparation; returns a job to inspect with job.read; commits only on verified success, which emits the event",
        retries: Owner,
    ),
    // The one job table belongs to the catalog owner, so the owner answers for every kind.
    owner!(
        JOB_READ,
        owner::JobParams,
        owner::job_read,
        "{job_id, kind, status, progress: {fraction?, message?}, asset_id?, module_id?, resource_id?, identity?, result?, error?: {code, message, data?}, request_id?} for a job of any kind: prepare, develop, artifacts and collect (source work), analysis, install, remove and task (capability work) or export; status is queued, running, ready, failed, cancelled or superseded; result is present only when ready: the prepared asset's state, a collection's counts, the analysis report, the capability job's value or the written export; a source or analysis job is read by the clients that requested it, and a capability or export job by any client; the owner keeps the last 64 finished source jobs, 32 of each other kind and 8 analysis reports"
    ),
    // The activity board belongs to the catalog owner, whose workers publish to it, so the owner
    // answers from it: one lock and a copy, nothing rendered or read.
    owner!(
        "activity.list",
        NoParams,
        owner::activity_list,
        "{sequence, active, recent, untracked}: the host's running work oldest first, and up to 16 recent entries that ran at least 250 ms, newest first; each entry has id, kind, label and elapsed_ms, or outcome (completed, cancelled or failed), duration_ms and ended_ms_ago, plus detail, asset_id, phase, progress {fraction, message} and job_id when known; job_id names the job job.read also answers, with the same status; sequence changes exactly when the contents do; needs no asset, takes no parameters, mutates nothing and emits no event"
    ),
    owner!(
        "job.adopt",
        owner::JobParams,
        owner::job_adopt,
        "select the ready result of this client's latest import as current; stale imports are refused"
    ),
    owner!(
        JOB_CANCEL,
        owner::JobParams,
        owner::job_cancel,
        "a source or analysis job: this client leaves it and still reads its outcome, and the work stops only when no other client wants it; a capability or export job: stops it for every client, a queued job never starts and a running one stops at its next checkpoint (an export removes its temporary file); a finished job is unchanged, so a repeated cancel changes nothing; returns the job as job.read does"
    ),
    owner!(
        "source.prepare",
        owner::SourcePrepare,
        owner::source_prepare,
        "queue signature-verified preparation of an imported source after reopen or cache eviction"
    ),
    service!(
        "catalog.list",
        CatalogList,
        |service, _, p| value(
            service.assets(p.after.as_ref(), p.limit.unwrap_or(DEFAULT_ASSET_PAGE))?
        ),
        "{assets, next}: one page of referenced assets in import order, each {id, locator, kind, width, height} read from the asset's own row without decoding its source interpretation; next is the after cursor of the following page, or null on the last; source.inspect reads one asset's full interpretation"
    ),
    service!(
        "asset.state",
        AssetParams,
        |service, _, p| value(service.state(&p.asset_id)?),
        "current entry, revision and redo path"
    ),
    service!(
        "source.inspect",
        SourceInspect,
        |service, _, p| service.inspect_source(&p.asset_id, p.entry_id.as_ref()),
        "persisted source identity, RAW interpretation, crop, backend and preparation readiness without decoding"
    ),
    service!(
        "history.list",
        HistoryList,
        |service, _, p| value(service.history(
            &p.asset_id,
            p.before_sequence,
            p.limit.unwrap_or(DEFAULT_HISTORY_PAGE)
        )?),
        "chronological entry rows newest first, including abandoned branches: identity, sequence, action, label, actor, time, undo parent and restore target, without the stack; history.inspect reads one whole entry"
    ),
    service!(
        "history.inspect",
        EntryParams,
        |service, _, p| value(service.entry(&p.asset_id, &p.entry_id)?),
        "one entry with its complete immutable stack"
    ),
    service!(
        "history.lineage",
        HistoryLineage,
        |service, _, p| value(service.lineage(
            &p.asset_id,
            p.entry_id.as_ref(),
            p.limit.unwrap_or(DEFAULT_HISTORY_PAGE)
        )?),
        "undo-parent chain newest first; next_entry_id continues a longer chain"
    ),
    service!(
        "recipe.describe",
        RecipeDescribe,
        |service, _, p| value(service.describe_entry(&p.asset_id, p.entry_id.as_ref())?),
        "an entry's stored layers in order with their module, title, summary, values, availability, whether each is neutral (changes nothing, by its module's own rule), input_stage, the {width, height} the layer receives from the layers before it (null after a layer whose output cannot be known), and input_orientation, the {mirror, turns} the orientation layers before it gave that stage (null with input_stage); then output_stage and output_orientation, what a layer appended to the stack would receive; reads and compiles payloads only and renders nothing"
    ),
    service!(
        "module.list",
        ModuleList,
        module_list,
        "every registered module descriptor with its effects, actions, parameters, controls and canvas declaration: the mode's title, shortcut letter and optional icon name, from the vocabulary an action control's icon uses. An effect lists the source kinds it may exist on as sources, omitted when it is every kind; a module applies to a photo when any of its effects does or it declares none, and asset_id keeps only the modules that apply to that asset's kind. A number, action or picker control, and a group's reset, may list variants [{source, module, control | reset}]: what another module provides in its place on the global target of a photo of that kind, returned unresolved"
    ),
    // Module settings are answered by the catalog owner, which holds the capability host: the
    // settings directory and the secret store. They are user-level, outside every catalog, and
    // never create history entries.
    owner!(
        settings::READ,
        ModuleParams,
        |owner, _, params| owner.host.read(owner.service.registry(), params),
        "{module_id, schema, revision, state, fields, profiles}: each field's value, default, source (user or default) and validity, a secret field as {secret_present} only, and each profile's status (ready, incomplete, missing-credentials or incompatible); state is ready, incomplete or incompatible"
    ),
    owner!(
        settings::SET,
        SetParams,
        |owner, call, params| {
            owner.host.settings_set(
                &mut owner.jobs,
                owner.service.registry(),
                params,
                &call.origin,
                &mut owner.announced,
            )
        },
        "validates the named non-secret fields against their declared parameters (a module's settings.fields and settings.profiles.fields are parameter descriptors, checked exactly as an action's are) and commits them together; null returns a field to its default; an endpoint is stored as the URL the transport policy accepts; a secret field is refused; mutation.expected_revision is the module's settings revision; returns {outcome, revision, changed, settings}",
        retries: Owner,
    ),
    owner!(
        settings::SET_SECRET,
        SetSecretParams,
        |owner, call, params| {
            owner.host.settings_set_secret(
                &mut owner.jobs,
                owner.service.registry(),
                params,
                &call.origin,
                &mut owner.announced,
            )
        },
        "stores one secret field's value in the secure store and never echoes it; a retry is matched by the setting alone; not-ready names a locked or unavailable store and nothing is kept in plain text",
        retries: Owner,
    ),
    owner!(
        settings::CLEAR_SECRET,
        ClearSecretParams,
        |owner, call, params| {
            owner.host.settings_clear_secret(
                &mut owner.jobs,
                owner.service.registry(),
                params,
                &call.origin,
                &mut owner.announced,
            )
        },
        "removes only that secret from the secure store; an absent secret is a no-op",
        retries: Owner,
    ),
    owner!(
        settings::RESET,
        ResetParams,
        |owner, call, params| {
            owner.host.settings_reset(
                &mut owner.jobs,
                owner.service.registry(),
                params,
                &call.origin,
                &mut owner.announced,
            )
        },
        "deletes the module's stored values and profiles and clears their secrets; the one write an incompatible entry accepts; the revision keeps counting",
        retries: Owner,
    ),
    owner!(
        settings::CREATE_PROFILE,
        CreateProfileParams,
        |owner, call, params| {
            owner.host.profile_create(
                &mut owner.jobs,
                owner.service.registry(),
                params,
                &call.origin,
                &mut owner.announced,
            )
        },
        "a new empty provider profile of a declared adapter with a host-generated profile-<uuid> identity, at most the module's declared maximum; returns it as profile",
        retries: Owner,
    ),
    owner!(
        settings::REMOVE_PROFILE,
        RemoveProfileParams,
        |owner, call, params| {
            owner.host.profile_remove(
                &mut owner.jobs,
                owner.service.registry(),
                params,
                &call.origin,
                &mut owner.announced,
            )
        },
        "clears the profile's secrets and removes it and its values, revokes the profile's grants and returns the removed profile",
        retries: Owner,
    ),
    // Permissions, resources and capability jobs are answered by the catalog owner too: grants
    // live beside the settings, and the jobs and lanes live there.
    owner!(
        grants::GRANT,
        GrantParams,
        |owner, call, params| {
            let authority = owner.authority(call.client);
            owner.host.grant(
                &owner.service,
                authority,
                params,
                &call.origin,
                &mut owner.announced,
            )
        },
        "grants one exact scope of a declared capability; only a client with permission authority may, otherwise forbidden; scope is {resource, version, origin} for download-artifact (the declared version from the origin of its pinned URL) or {profile_id, adapter, origin, data, asset_id} for remote-image-request (an existing profile of that adapter whose endpoint has that origin, the capability's data class and an asset of this catalog); clears a matching denial; a scope that already has a live grant returns it as a no-op; returns {grant, outcome, deduplicated}",
        retries: Owner,
    ),
    owner!(
        grants::DENY,
        DenyParams,
        |owner, call, params| {
            owner.host.deny(
                owner.service.registry(),
                params,
                &call.origin,
                &mut owner.announced,
            )
        },
        "records that the person did not allow one exact scope; the next consent-required for it reports denied: true; any client may; returns {denial, deduplicated}",
        retries: Owner,
    ),
    owner!(
        grants::REVOKE,
        RevokeParams,
        |owner, call, params| {
            owner.host.revoke(
                &mut owner.jobs,
                params,
                &call.origin,
                &mut owner.announced,
            )
        },
        "marks the grant revoked and cancels the queued and running jobs that depend on it with cancelled: permission revoked; never touches recipes, history or artifacts; any client may; returns {grant, outcome, cancelled_jobs, deduplicated}",
        retries: Owner,
    ),
    owner!(
        grants::LIST,
        PermissionList,
        |owner, _, params| owner
            .host
            .list_permissions(owner.service.registry(), params),
        "{grants, denials}: every grant, revoked ones with {revoked: {ms, reason}}, and every recorded denial; none holds a secret"
    ),
    owner!(
        host::STATUS,
        ModuleParams,
        |owner, _, params| owner
            .host
            .status(&owner.jobs, owner.service.registry(), params),
        "{module_id, settings: {state, revision, missing}, resources, permissions: {live, revoked, denials}, jobs}; permissions counts the module's grants and denials, whose records module.permission.list returns; reads settings, stats installed markers and reads grants, and loads nothing"
    ),
    owner!(
        resources::RESOURCE_LIST,
        ModuleParams,
        |owner, _, params| owner
            .host
            .resource_list(&owner.jobs, owner.service.registry(), params),
        "{resources: [{id, title, version, bytes, sha256, license, provenance, url, state, path?, installed_ms?, job_id?, error?}], storage: {root, used_bytes, quota_bytes}}; state is not-installed, installing, installed or failed; stats only, no hashing"
    ),
    owner!(
        resources::INSTALL,
        InstallParams,
        |owner, call, params| {
            owner.host.install(
                &mut owner.jobs,
                owner.service.registry(),
                params,
                &call.origin,
            )
        },
        "queues a transfer-lane job that streams into staging, checks the pinned length and SHA-256, asks the module to check the format, checks the storage quota and only then installs; consent-required and resource-limit are reported before anything is queued; an installed resource answers state: installed and a second request joins the running install; returns {module_id, resource_id, state, job_id?, status?, deduplicated}",
        retries: Owner,
    ),
    owner!(
        resources::REMOVE,
        ResourceParams,
        |owner, call, params| {
            owner.host.remove(
                &mut owner.jobs,
                owner.service.registry(),
                params,
                &call.origin,
            )
        },
        "queues a transfer-lane job that deletes the installed version; never touches a catalog, recipe or artifact; returns {module_id, resource_id, state, job_id?, status?, deduplicated}",
        retries: Owner,
    ),
    mutating!(
        "history.undo",
        Navigate,
        history_undo,
        "moves current to its undo parent without adding an entry",
        retries: Catalog,
    ),
    mutating!(
        "history.redo",
        Navigate,
        history_redo,
        "follows the persisted redo path",
        retries: Catalog,
    ),
    mutating!(
        "history.restore",
        Restore,
        history_restore,
        "appends a restore action copying the entry's stack and returns the session to current",
        retries: Catalog,
    ),
    mutating!(
        "version.create",
        VersionCreate,
        version_create,
        "names a retained entry; unique per asset ignoring case; no-op when the name already names that entry; records mutation.actor",
        retries: Owner,
    ),
    mutating!(
        "version.delete",
        VersionDelete,
        version_delete,
        "removes the name only; the entry stays in history; no-op when absent",
        retries: Owner,
    ),
    service!(
        "version.list",
        AssetParams,
        |service, _, p| Ok(json!({"versions": service.versions(&p.asset_id)?})),
        "saved versions in creation order with their entry sequence"
    ),
    // The preset library is catalog data beside history. None of these methods renders, opens a
    // source or hashes pixels; applying a preset is edit.apply-preset.
    service!(
        "preset.list",
        NoParams,
        |service, _, _| Ok(json!({"presets": service.presets()?})),
        "{presets: [{id, name, group, settings, origin, report, actor, created_ms, updated_ms, unavailable}]} sorted by group, then name, ignoring case; report is the import report's counts {mapped, neutral, unsupported, refused}, or null for a preset created in Luxforge; unavailable names the settings actions this registry cannot apply; no source_text"
    ),
    service!(
        "preset.read",
        PresetParams,
        preset_read,
        "{preset}: one record with its full import report and source_text, the imported file's text kept verbatim, or null for a preset created in Luxforge"
    ),
    mutating!(
        "preset.create",
        PresetCreate,
        preset_create,
        "{preset, deduplicated}: stores a settings set as a Luxforge preset with origin {kind: luxforge}, report null and mutation.actor as its actor; name is 1..128 and group 1..64 printable characters after trimming, the (group, name) pair is unique ignoring case (a duplicate is a conflict) and the library holds at most 1000 presets (resource-limit); every action and field is checked against the registry",
        retries: Owner,
    ),
    service!(
        "preset.capture",
        PresetCapture,
        preset_capture,
        "{settings} read from one entry's stack: fields maps field-patch actions to an array of their parameter names or true for all of them; each field takes the value of its module's one layer, or its declared default when the stack has none; two or more layers are validation: ambiguous; reads stored payloads only, so it opens no source and renders nothing; send the result to preset.create"
    ),
    mutating!(
        "preset.update",
        PresetUpdate,
        preset_update,
        "{outcome, preset, deduplicated}: applied when the name, group or settings change, recording mutation.actor and updated_ms; no-op, with nothing written, when they do not; a rename onto another preset's (group, name) pair is a conflict; origin and report are kept",
        retries: Owner,
    ),
    mutating!(
        "preset.delete",
        PresetDelete,
        preset_delete,
        "{outcome, deleted, deduplicated}: applied and true when the preset existed, no-op and false when it is absent; history entries that applied it are unchanged",
        retries: Owner,
    ),
    service!(
        "preset.export",
        PresetParams,
        |service, _, p| value(service.export_preset(&p.preset_id)?),
        "{file_name, content}: the preset as a Luxforge preset document named <name>.lfpreset, which preset.import reads back to the same name, group and settings"
    ),
    service!(
        "preset.inspect",
        PresetInspect,
        |service, _, p| service.inspect_import(&p.content, p.file_name.as_deref()),
        "dry run of preset.import that stores nothing: {preset, report}, where preset has the record's shape with id, actor, created_ms and updated_ms null, the file's name and the file's group or Imported, and report is the full per-setting import report; a file that maps nothing still returns its report with empty settings"
    ),
    mutating!(
        "preset.import",
        PresetImport,
        preset_import,
        "{preset, report, deduplicated}: reads the text of a Luxforge preset document, a Lightroom XMP preset or a .lrtemplate, at most 1 MiB, and stores its mapped settings with the text kept verbatim and mutation.actor as its actor; the library's name, uniqueness and size rules apply as for preset.create; a file that maps nothing is unsupported-input with the report counts, and a refused import stores nothing",
        retries: Owner,
    ),
    service!(
        "preview.select",
        PreviewSelect,
        preview_select,
        "read-only session selection of one asset, kept per asset in session.preview.selections so a client may preview history on one photo while it edits another; an asset with a selection refuses edits from this client until it returns to current; the current entry selects current, not a historical preview, and returns only that asset; at most 16 assets have a selection at once (resource-limit past it); keep_geometry frames the selected entry with the geometry layers of the entry the session displays of that asset now, recorded as the selection's geometry_from, and the preview, render.sample, render.locate and render.transform of that asset's selection follow it; returns generation and session"
    ),
    service!(
        "preview.return-current",
        NoParams,
        preview_return_current,
        "returns every asset to current; returns generation and session"
    ),
    service!(
        "view.set",
        ViewSet,
        view_set,
        "session view state; returns the session"
    ),
    service!(
        "workspace.set",
        WorkspaceSet,
        workspace_set,
        "per-client screen preference: panels, canvas mode and overlays; needs no asset and changes no history or frame; returns the session"
    ),
    service!(
        "session.state",
        NoParams,
        |service, session, _| session_value(service, session),
        "this client's selection, view, workspace state and session revision"
    ),
    service!(
        "resources.read",
        NoParams,
        |service, _, _| value(crate::resources::read(service.render_context())),
        "what the operating system accounts to this process: {monotonic_ns, cpu: {time_ns, logical_cpus}, memory: {kind, bytes, peak_bytes, resident_bytes}, gpu: {time_ns, allocated_bytes, unified_memory}, budgets: {colour_scratch, spatial} each {target_bytes, in_use_bytes, peak_bytes}}; times are nanoseconds and sizes bytes; memory.kind is footprint (macOS, Activity Monitor's Memory, including GPU allocations on unified memory), resident (Linux) or private (Windows); time counters are cumulative, so a rate comes from two reads: CPU percent of one core is 100 × Δcpu.time_ns / Δmonotonic_ns, up to 100 × logical_cpus, and GPU percent is the same over gpu.time_ns; monotonic_ns means something only as a difference; a counter the platform cannot give is omitted and its object's unavailable maps its key to the reason; takes no parameters, needs no asset, emits no event and changes nothing"
    ),
    service!(
        "draft.begin",
        DraftBegin,
        draft_begin,
        "opens this client's one draft of that action, bound to the asset's current revision; refused while a draft is open or a historical entry is previewed, and, in its commit's words, for an action whose module does not apply to the asset's source kind"
    ),
    service!(
        "draft.set",
        DraftSet,
        draft_set,
        "validates the named fields against the action's parameters and merges them into the draft; an invalid field, or one superseded on the draft's target (refused in its commit's words), changes nothing"
    ),
    service!(
        "draft.read",
        DraftParams,
        draft_read,
        "the draft with conflicted recomputed against the asset's current revision"
    ),
    service!(
        "draft.cancel",
        DraftParams,
        draft_cancel,
        "ends the draft and commits nothing"
    ),
    mutating!(
        "draft.commit",
        DraftCommit,
        draft_commit,
        "runs the draft's action with its accumulated fields and ends the draft; a conflicted draft or a mismatched expected_revision is refused and the draft is kept",
        retries: Owner,
    ),
    service!(
        "draft.reapply",
        DraftParams,
        draft_reapply,
        "rebases the draft on the asset's current revision, keeping and revalidating only the fields this client set"
    ),
    // Planned on the owner; a sample through a spatial layer is evaluated on the point worker.
    planned!(
        "render.sample",
        RenderSample,
        render_sample,
        "one pixel of the session's selected entry, or of an open draft's effective recipe, evaluated without rasterizing; names the entry and snapshot it was planned against, and its response sequence is the event sequence when it was planned; a sample through a spatial layer is evaluated off the catalog owner, and resource-limit means too many such samples are already waiting"
    ),
    service!(
        "render.locate",
        RenderLocate,
        render_locate,
        "the content pixel, the source after EXIF orientation, that one output pixel shows"
    ),
    service!(
        "render.transform",
        RenderTransform,
        render_transform,
        "the geometry tail as one affine map, {content, output, forward, inverse}, in continuous pixel-center coordinates, so a gesture maps pointer positions between the content stage and the rendered image without asking per move"
    ),
    owner!(
        "events.since",
        owner::EventsSince,
        owner::events_since,
        "gap=true requires an asset.state refresh"
    ),
    owner!(
        "events.wait",
        owner::EventsWait,
        owner::events_wait,
        "a long poll that answers as events.since does, {events, current_sequence, gap}, as soon as the log holds an event after `after` (one naming asset_id when it is given) or gap is true, and otherwise, when timeout_ms (0 to 30000, default 10000) passes, with no events; 0 answers at once; the owner holds the wait without blocking, at most one per client, so a second events.wait from a client answers its earlier one at once with what it has, and a disconnect drops it; a JSON connection serves one request at a time, so a client that waits on it uses a second connection for anything else"
    ),
    // The analysis methods are answered by the catalog owner, because the job store, the worker
    // slots and every client's draft live there. They mutate nothing and emit no event.
    owner!(
        "analysis.request",
        owner::AnalysisRequest,
        owner::analysis_request,
        "queues the exact RGB histogram and output-clipping reduction of one evaluated stack and returns the job as job.read does, promptly, with the report as its result when one is already kept; target is {kind:current}, {kind:entry,entry_id} or {kind:draft,draft_id} for this client's own draft; identical identities share one job; only ready carries counts, so no status can be read as an empty histogram"
    ),
    // JPEG export (`docs/design/export.md`): the owner plans in O(layers) and checks the
    // destination; the render, encode and write run on its export lane.
    owner!(
        "export.plan",
        owner::export::ExportPlanParams,
        owner::export::plan,
        "{asset_id, entry_id, snapshot_id, width, height, suggested}: the output stage of a saved entry's compiled recipe, the current entry unless entry_id names another, and suggested, an absolute path <original stem>-edited.jpg (or -edited-2.jpg ...) beside the original that does not exist yet, or null; renders and prepares nothing; a stack the host cannot evaluate is refused with its reason"
    ),
    owner!(
        "export.jpeg",
        owner::export::ExportJpeg,
        owner::export::jpeg,
        "writes one saved entry's exact render, the current entry unless entry_id names another, to a new baseline quality-90 sRGB JPEG at destination: an absolute path ending .jpg or .jpeg whose parent directory exists and at which nothing exists (conflict otherwise, and nothing is ever replaced); keep_metadata writes the original's supported EXIF fields, otherwise the file carries none; the entry is frozen when accepted, so later commits never change it; queues one job on the export lane (one running, four waiting, resource-limit beyond), read with job.read and cancelled with job.cancel, and returns {job_id, status, asset_id, entry_id, snapshot_id, destination, width, height, keep_metadata, deduplicated}; an unprepared source is preparation-required with its job; a finished export records an event",
        retries: Owner,
    ),
    service!(
        "artifact.status",
        NoParams,
        |service, _, _| service.artifact_status(),
        "the catalog's derived-artifact root and its state (absent, ready, missing or foreign), the catalog_id its manifest must name, and how many artifacts entries reference, how many a collection would remove and their recorded bytes; reads the manifest and counts rows only"
    ),
    service!(
        "artifact.inspect",
        ArtifactInspect,
        |service, _, p| service.inspect_artifact(&p.artifact_id),
        "one artifact's record (hash, bytes, kind, dimensions, colour, publishing module, time), whether its file is present, missing or of the wrong length, how many entries reference it and whether a task of this process published it; stats only"
    ),
    // Collection runs on the source worker and is read with job.read, so the catalog owner
    // answers it. It emits its event when the request is accepted.
    owner!(
        "artifact.collect",
        owner::Collect,
        owner::artifact_collect,
        "removes the rows of artifacts no entry references and no task of this process published, then queues a source job that removes their files, object files without a row and staged files older than an hour; nothing an entry references is touched; returns {job_id, status, deduplicated} and the job result counts {rows, objects, temporary}",
        retries: Owner,
    ),
    // The catalog's methods, one marked section per lane. Each is declared once in
    // `catalog_types::api` (its parameter struct, answer, errors and notes) and registered here by
    // its lane when it works; name the parameter struct and the handler by path
    // (`crate::catalog_types::api::PickSet`, `owner::library::pick_set`) so no lane edits the
    // imports above. A mutation declares `retries: Owner`.
    // ── catalog lane A: files ──
    // The index lane (TASK-004): volumes, folders on disk, indexed folders and listings.
    owner!(
        "volume.list",
        params::NoParams,
        owner::files::volume_list,
        "{volumes: [{volume: {id, mount_point, label, removable, platform_id?, last_seen_ms}, offline, card, startup}]}: the mounted volumes, the startup disk first, each with whether it is a card (a DCIM folder at its root), then the volumes the catalog knows that are not mounted, offline; reads the mount table without waiting on any file system"
    ),
    owner!(
        "disk.folders",
        crate::catalog_types::api::DiskFoldersParams,
        owner::files::disk_folders,
        "{path, folders: [{name, path}], truncated}: the immediate subfolders of an absolute folder in name order, without hidden and system folders, packages, other applications' caches and Luxforge's own directories, following no link; at most 2,000 folders from 50,000 entries, truncated past them; validation for a file, a package or a cache"
    ),
    owner!(
        "card.list",
        params::NoParams,
        owner::files::card_list,
        "{cards: [{volume, dcim, files?, cameras}]}: the mounted volumes with a DCIM folder at their root; files is what the last listing of the card found and cameras the bodies its headers name (at most 16)"
    ),
    owner!(
        "index.folders",
        params::NoParams,
        owner::files::index_folders,
        "{folders: [{path, volume_id, added_ms, actor, offline, files?, listed_ms?}]}: the indexed folders in path order; offline when a folder is not there and its volume is not mounted; files and listed_ms from its last complete listing"
    ),
    owner!(
        "index.refresh",
        crate::catalog_types::api::IndexRefresh,
        owner::files::index_refresh,
        "lists a source again — {kind: indexed-folder, path}, {kind: card, volume_id}, {kind: folder, path} or {kind: all-indexed} — as an index-refresh job, answering {job_id, status, deduplicated} at once (a source already waiting or running answers its job); the job reads the headers of new and changed files only, carries moved files' rows, drops vanished ones and reports {roots, files, added, changed, moved, removed, unreadable, headers_read, offline?, missing?}; follows no link, crosses no volume, skips packages, caches, hidden and system folders and Luxforge's own directories; resource-limit past 500,000 files or 16 waiting listings; source-unavailable for an offline or missing folder or card; progress on the activity board, cancelled with job.cancel keeping what was committed; each committed batch records one event naming the index_revision it left"
    ),
    owner!(
        "index.add-folder",
        crate::catalog_types::api::IndexAddFolder,
        owner::files::index_add_folder,
        "adds a folder, with its subfolders, to the indexed folders as one library change and starts listing it, answering {change: {outcome, change?, items, deduplicated}, folder: {path, volume_id, added_ms, actor, offline, files?, listed_ms?}, job_id?}; a folder already indexed changes nothing; validation for a relative path, a file, a package, another application's cache or Luxforge's own directories; conflict, naming it in data.folder, for a folder inside or around an indexed one; library.undo removes it and forgets its rows",
        retries: Owner,
    ),
    owner!(
        "index.remove-folder",
        crate::catalog_types::api::IndexRemoveFolder,
        owner::files::index_remove_folder,
        "removes an indexed folder as one library change, answering {outcome, change?, items, deduplicated}: its listing stops and its rows are forgotten unless another root lists them; nothing on disk changes; a folder that is not indexed changes nothing; library.undo adds it back and lists it again",
        retries: Owner,
    ),
    // ── end lane A ──
    // ── catalog lane B: previews ──
    owner!(
        "preview.read",
        crate::catalog_types::api::PreviewRead,
        owner::previews::preview_read,
        "a cached preview's path, size and origin, or the job that makes it and the best preview cached meanwhile; a file's tiers are grid (at most 512 px, first its thumbnail stage, origin exif-thumbnail, then its embedded preview) and loupe (its largest embedded preview, at most 2560 px, never enlarged), every one upright; the job (preview-extract) is shared by every request for the same file and tier, its result is the preview, and job.cancel removes or stops it; a file the index does not hold, or tier large, is validation; a file with no usable preview is unsupported-input naming why; resource-limit past 20,000 queued tasks; item photo is unsupported-input until rendered previews of developed photographs are built"
    ),
    // ── end lane B ──
    // ── catalog lane C: catalog ──
    // Picks and the library journal (TASK-011).
    owner!(
        "pick.set",
        crate::catalog_types::api::PickSet,
        owner::library::pick_set,
        "picks or clears files as one library change, answering {outcome, change?, items, deduplicated}; a pick keeps the file's path and signature with its actor, request and time until it is developed or cleared, and picking a file already picked keeps its pick; targets are files by path or index row, or the files selected in the caller's view; resource-limit past 50,000 files",
        retries: Owner,
    ),
    owner!(
        "pick.list",
        crate::catalog_types::api::PickList,
        owner::library::pick_list,
        "{picks: [{path, signature, volume_id, actor, request_id, picked_ms, file_id?}], next_after?}: the picks in path order after the path after, every pick or those of a folder (with its subfolders when asked) or a card; file_id is the file's index row when the index lists it"
    ),
    owner!(
        "library.journal",
        crate::catalog_types::api::LibraryJournalParams,
        owner::library::library_journal,
        "{changes: [{sequence, actor, request_id, method, label, time_ms, item_count, undoes?, redoes?, undone_by?}], next_after?}: library changes after the sequence after, oldest first, without their rows; next_after continues a full page"
    ),
    owner!(
        "library.inspect",
        crate::catalog_types::api::LibraryInspect,
        owner::library::library_inspect,
        "{change, rows: [{item: {kind, ...}, before, after}]}: one library change with each item's value before and after, null for absent, in the order the change applied them"
    ),
    owner!(
        "library.undo",
        crate::catalog_types::api::LibraryRequest,
        owner::library::library_undo,
        "reverts the calling actor's latest library change not yet undone by appending its inverse, answering {outcome, change?, items, deduplicated}, no-op when there is none; conflict, naming the items in data.items, when a later change touched them or their values are no longer the ones it left",
        retries: Owner,
    ),
    owner!(
        "library.redo",
        crate::catalog_types::api::LibraryRequest,
        owner::library::library_redo,
        "reverts the calling actor's latest undo not yet redone, made since its latest new change, under the same rule as library.undo",
        retries: Owner,
    ),
    owner!(
        "catalog.info",
        params::NoParams,
        owner::library::catalog_info,
        "{path, catalog_id, format, index_format, counts: {photographs, recently_developed, removed, unavailable, folders, collections, picks, indexed_folders, library_changes}, index: {path, bytes, files}, previews: {path, bytes, files}}: the catalog's path, identity and formats, the counts behind the Catalog sources (photographs and unavailable ones exclude the removed; recently developed is the last 30 days), the index database's size and the files it lists, and the preview cache's bytes and files as its rows record them"
    ),
    // Catalog folders and collections (TASK-012).
    owner!(
        "folder.list",
        params::NoParams,
        owner::library::organize::folder_list,
        "{folders: [{id, name, parent_id?, created_ms, event?: {start_ms, end_ms, event_id?}, count, year?}]}: every catalog folder, parents before children and siblings by name ignoring case; count is the photographs directly in it, removed ones excepted, and year the capture year of its earliest dated photograph"
    ),
    owner!(
        "folder.create",
        crate::catalog_types::api::FolderCreate,
        owner::library::organize::folder_create,
        "makes an empty catalog folder in parent_id's folder or at the top level as one library change, answering {change: {outcome, change?, items, deduplicated}, folder, deduplicated}; the name is trimmed, 1 to 128 characters without control characters (validation), and unique among its siblings ignoring case (conflict naming the clash); an unknown parent is validation; nothing on disk changes",
        retries: Owner,
    ),
    owner!(
        "folder.rename",
        crate::catalog_types::api::FolderRename,
        owner::library::organize::folder_rename,
        "renames a catalog folder as one library change, answering {outcome, change?, items, deduplicated}; the name follows folder.create's rules (validation, conflict naming the clash), an unknown folder is validation and the same name is a no-op; nothing on disk changes",
        retries: Owner,
    ),
    owner!(
        "folder.move",
        crate::catalog_types::api::FolderMove,
        owner::library::organize::folder_move,
        "nests a catalog folder in parent_id's folder, or at the top level without it, as one library change, answering {outcome, change?, items, deduplicated}; a move into itself or a folder inside it, and an unknown folder or parent, are validation; a sibling of the same name ignoring case is conflict",
        retries: Owner,
    ),
    owner!(
        "folder.merge",
        crate::catalog_types::api::FolderMerge,
        owner::library::organize::folder_merge,
        "moves every photograph of folder_id, removed ones too, into into_id, then its subfolders under into_id, then deletes folder_id, as one library change whose undo restores all three, answering {outcome, change?, items, deduplicated}; a merge into itself or a folder inside it and an unknown folder are validation; a subfolder whose name into_id already holds is conflict naming it; more than 50,000 items is resource-limit",
        retries: Owner,
    ),
    owner!(
        "folder.delete",
        crate::catalog_types::api::FolderDelete,
        owner::library::organize::folder_delete,
        "deletes an empty catalog folder as one library change, answering {outcome, change?, items, deduplicated}; a folder any photograph (removed ones included) or folder is in is conflict saying what it holds; an unknown folder is validation",
        retries: Owner,
    ),
    owner!(
        "asset.move",
        crate::catalog_types::api::AssetMove,
        owner::library::organize::asset_move,
        "moves the photographs targets names into the catalog folder folder_id as one library change, answering {outcome, change?, items, deduplicated}; photographs already there are left out, and none to move is a no-op; targets are photographs by asset_ids, by their originals' paths or index rows, or the photographs selected in the caller's view; an unknown folder or photograph is validation; resource-limit past 50,000; nothing on disk changes",
        retries: Owner,
    ),
    owner!(
        "collection.list",
        params::NoParams,
        owner::library::organize::collection_list,
        "{collections: [{id, name, parent_id?, kind: collection | smart | group, query?, created_ms, count?}]}: every collection, smart collection and group, parents before children and siblings by name ignoring case; count is a plain collection's members, removed photographs excepted, and query a smart collection's stored query"
    ),
    owner!(
        "collection.create",
        crate::catalog_types::api::CollectionCreate,
        owner::library::organize::collection_create,
        "makes a plain collection (kind collection) or a group (kind group) in the group parent_id or at the top level as one library change, answering {change: {outcome, change?, items, deduplicated}, collection, deduplicated}; the name is trimmed, 1 to 128 characters without control characters (validation), and unique among its siblings of every kind ignoring case (conflict naming the clash); a parent that is unknown or not a group is validation",
        retries: Owner,
    ),
    owner!(
        "collection.create-smart",
        crate::catalog_types::api::CollectionCreateSmart,
        owner::library::organize::collection_create_smart,
        "saves query as a smart collection in the group parent_id or at the top level as one library change, answering as collection.create; the query is stored as given with its defaults and must be one browse.view accepts over photographs: an event, folder or card source (files) is validation, as is a {kind: collection} source naming a smart collection, a group or an unknown collection, and a {kind: catalog-folder} source naming an unknown folder; names as collection.create",
        retries: Owner,
    ),
    owner!(
        "collection.update-smart",
        crate::catalog_types::api::CollectionUpdateSmart,
        owner::library::organize::collection_update_smart,
        "replaces a smart collection's query as one library change, answering {outcome, change?, items, deduplicated}; the query is checked as collection.create-smart's; a collection that is unknown or not smart is validation",
        retries: Owner,
    ),
    owner!(
        "collection.rename",
        crate::catalog_types::api::CollectionRename,
        owner::library::organize::collection_rename,
        "renames a collection, smart collection or group as one library change, answering {outcome, change?, items, deduplicated}; the name follows collection.create's rules (validation, conflict naming the clash); an unknown collection is validation and the same name a no-op",
        retries: Owner,
    ),
    owner!(
        "collection.move",
        crate::catalog_types::api::CollectionMove,
        owner::library::organize::collection_move,
        "moves a collection, smart collection or group into the group parent_id, or to the top level without it, as one library change, answering {outcome, change?, items, deduplicated}; a parent that is unknown or not a group, and a group moved into itself or a group inside it, are validation; a sibling of the same name ignoring case is conflict",
        retries: Owner,
    ),
    owner!(
        "collection.delete",
        crate::catalog_types::api::CollectionDelete,
        owner::library::organize::collection_delete,
        "deletes a plain collection with its memberships, a smart collection, or an empty group, as one library change whose undo restores the collection and its members, answering {outcome, change?, items, deduplicated}; a group that holds anything is conflict; an unknown collection is validation; a collection with 50,000 or more members is resource-limit",
        retries: Owner,
    ),
    owner!(
        "collection.add",
        crate::catalog_types::api::CollectionMembers,
        owner::library::organize::collection_add,
        "adds the photographs targets names to a plain collection as one library change, answering {outcome, change?, items, deduplicated}; a member keeps when it joined and none to add is a no-op; targets as asset.move's; a collection that is unknown, smart or a group, and an unknown photograph, are validation; resource-limit past 50,000",
        retries: Owner,
    ),
    owner!(
        "collection.remove",
        crate::catalog_types::api::CollectionMembers,
        owner::library::organize::collection_remove,
        "removes the photographs targets names from a plain collection as one library change, answering {outcome, change?, items, deduplicated}; photographs not in it are left out and none to remove is a no-op; refused as collection.add",
        retries: Owner,
    ),
    // Availability and Locate (TASK-016).
    owner!(
        "source.check",
        crate::catalog_types::api::SourceCheck,
        owner::library::sources::source_check,
        "starts a source-check job, answering {job_id, status, deduplicated}, whose result is {rows: [{asset_id, availability, checked_ms}]}: each photograph's original as found now, available (its recorded file at its path), offline (its volume is not connected: one look at the mount point covers every photograph on it), missing (no file at its path) or changed (another file there), recorded with the time it was looked at, outside the journal; a photograph whose file moved within its volume is found again by its file identity in the index, confirmed by its fingerprint and relinked as one library change by the actor system, and nothing else is relinked; one event when anything changed, naming the change when there is one; targets are photographs by id, by the path of their original or its index row, or the photographs selected in the caller's view; read and cancel the job with job.read and job.cancel; resource-limit past 50,000 photographs or when 4 library jobs already wait"
    ),
    owner!(
        "source.locate",
        crate::catalog_types::api::SourceLocate,
        owner::library::sources::source_locate,
        "starts a source-locate job, answering {job_id, status, deduplicated}, whose result is {outcome, change?, items, deduplicated}: the chosen file's SHA-256 is streamed off the owner, cancellable, and must equal the photograph's fingerprint while the file keeps its signature throughout; the photograph then points at it as one library change (asset-source, undone with library.undo), its volume recorded and its original available, with its history, edits and fingerprint unchanged; refused before anything is read: a relative path or a folder (validation), a file that cannot be read (read-error), one of another length (source-unavailable) and one another photograph names (conflict, naming it in data.asset_id; photographs are never merged); the job fails with source-unavailable when the bytes differ and conflict when the file changes during or after verification or another photograph names it by then; a cancel, a mismatch, an unplugged volume or a failed commit changes nothing; resource-limit when 4 library jobs already wait",
        retries: Owner,
    ),
    // Developing picks (TASK-013).
    owner!(
        "pick.plan",
        crate::catalog_types::api::PickPlan,
        owner::library::develop::pick_plan,
        "{events: [{event_id?, name, count, folder: {kind: existing, folder_id} | {kind: new, name, parent_id?}, folder_name?, removable?: [{volume_id, label, count, with_copy}]}], count, offline}: what developing the files targets names, picked or not, would do (without targets, every pick in the caller's view, which is validation until views land): the files by event, the events organized over every file the index lists in their folders (a file it does not list is an undated frame of its folder), chronological and undated last; each event's proposed folder, the newest catalog folder made from it (its stored event key, else an overlapping stored span; for an undated event, the folder its folder's undated photographs went into) with its folder_name, or else a new top-level folder named after the event (its place with the month and year of its first day, \"Konstanz · Sep 2026\", else the event's name), unique among the top-level folders; the picks on each removable volume and how many have a file of the same name and length in an indexed folder on a fixed volume (with_copy); and offline, the files whose volume is not connected; reads nothing but the catalog, the index and one stat a file"
    ),
    owner!(
        "pick.develop",
        crate::catalog_types::api::PickDevelop,
        owner::library::develop::pick_develop,
        "starts a develop-picks job, answering {job_id, status, deduplicated}, whose result is {developed: [{path, used?, asset_id, outcome: created | linked | relinked}], failed: [{path, asset_id?, code, message}], changes}: the files are planned as pick.plan plans them and each event goes into the folder its into entry names by event_id, else the entry with no event_id, else the plan's proposal, so into: [] accepts the plan; an existing folder must exist and a new one's name is checked as folder.create's (validation, conflict), and events given the same new folder share it, made with their span; each file is read once off the owner, bounded, its SHA-256 streamed, its header read and its interpretation read without developing, and must keep its signature while it is read and until it commits; a file whose bytes are a photograph's is linked to it, one whose name, length and fingerprint match a photograph whose original is not there relinks it (asset-source, its folder and history unchanged), a file another photograph names with other bytes fails with conflict, and any other becomes a photograph with its Original, capture row, source folder and moment in its event's folder; the job commits in batches, the first of one file, then up to 100 files or 5 s, never across an event, each one library change (developed-asset, catalog-folder, asset-source and pick items) announced as one event, clearing its picks; a file that fails is listed in failed and stays picked; picks on a removable volume are conflict (naming it in data) unless use_copies finds each a copy to verify or confirm_removable is set, and with use_copies a card's pick is developed from the first copy whose fingerprint matches (used), else from the card only with confirm_removable; an offline file fails with source-unavailable; nothing to develop is validation; job.cancel stops between files and keeps every batch committed; library.undo of a batch sends its photographs back and picks their files again; a retry answers the first job, and after a restart a finished job with the report its changes record; resource-limit when 4 library jobs already wait",
        retries: Owner,
    ),
    owner!(
        "asset.send-back",
        crate::catalog_types::api::AssetTargets,
        owner::library::develop::asset_send_back,
        "sends the photographs targets names back as one library change, answering {outcome, change?, items, deduplicated}: each one's catalog record (asset, capture, Original entry, state and requests) is deleted and its file picked again with its signature now (kept as picked when it already is); refused with conflict, naming the item in data.items and changing nothing, for a photograph with history beyond its Original, a named version or a collection (it leaves only by removal) and for one whose original is not at its locator (it could not be picked again); a sent-back photograph is developed again with pick.develop, so undoing a send-back is conflict; targets as asset.move's; the file is never touched",
        retries: Owner,
    ),
    // Resolving missing originals (TASK-017).
    owner!(
        "source.missing",
        crate::catalog_types::api::SourceMissing,
        owner::library::missing::source_missing,
        "{groups: [{source_folder, volume_id, count, catalog_folders: [{id, name}], reason: {kind: volume-offline, label} | {kind: folder-gone} | {kind: files-gone} | {kind: changed}}], count}: every photograph not removed whose original was last recorded offline, missing or changed (by source.check or a refusal; one never checked is not listed), grouped by the folder on disk it was developed from, in folder order, with the catalog folders its photographs are in now; the reason is volume-offline when the group's volume is not mounted (one look at its mount point), folder-gone when the volume is and the folder is not, changed when every photograph in it was recorded changed, and files-gone otherwise, a group mixing missing and changed originals included; nothing is looked at per file"
    ),
    owner!(
        "source.find",
        crate::catalog_types::api::SourceFind,
        owner::library::missing::source_find,
        "starts a source-find job, answering {job_id, status, deduplicated}, whose result is {rows: [{asset_id, file_name, result: found {path} | several-identical {paths} | different-bytes {path} | claimed {path, by} | not-found}]} and which changes nothing: search_root (an absolute folder) is walked with its subfolders, following no symbolic link, entering no other volume and skipping hidden folders, packages and other applications' caches, for files of each photograph's original's name (ignoring case on macOS and Windows) and length, and each candidate's SHA-256 is streamed off the owner, cancellable, one file at a time; found is one file with the original's bytes, several-identical more than one (a choice to make), claimed a file with its bytes that another photograph already names (never taken), different-bytes a file of the same name whose bytes differ, left as it is; while the job runs, job.read's result is the report so far with result checking for photographs not yet looked at, and its progress counts files looked at, then photographs checked; the files found and several-identical name are remembered for source.relink; exactly one of targets (photographs, as source.check's) and source_folder (every missing photograph developed from that folder, as source.missing lists it) is validation otherwise; a relative path or a file as search_root is validation and one that cannot be read read-error; the job fails with read-error for a folder or file it cannot read, source-unavailable when the folder's drive is disconnected, and resource-limit past 500,000 files or 100,000 folders; resource-limit past 50,000 photographs or when 4 library jobs already wait"
    ),
    owner!(
        "source.relink",
        crate::catalog_types::api::SourceRelink,
        owner::library::missing::source_relink,
        "points every photograph of pairs at its file in one transaction as one library change (asset-source items, labelled Relinked <file> or Relinked N originals, undone with library.undo), answering {outcome, change?, items, deduplicated}: each photograph's source folder becomes its file's folder, its volume is recorded and its original recorded available, and its history, edits and fingerprint are unchanged; a pair whose photograph already names its file needs nothing; every other pair must be a file a finished source.find verified for that photograph, still with the signature it had then and named by no other photograph, or nothing changes and the request is refused naming every such pair in data.pairs [{asset_id, path, reason: not-verified | changed | gone | claimed | same-file, by?}] (the first 100, with data.count): source-unavailable when a file is gone, conflict otherwise; a relative path, an unknown photograph, and a photograph or file named twice are validation; more than 50,000 pairs is resource-limit",
        retries: Owner,
    ),
    // Removing (TASK-014).

    // Batch preset and export (TASK-015).

    // ── end lane C ──
    // ── catalog lane D: views ──
    // ── end lane D ──
];

/// A resolved method: a host method from the static table, or one generated from an action, query
/// or task a registered module or the host descriptor declares. All of them come from the same
/// lookup discovery uses.
pub(super) enum Method {
    Host(&'static MethodSpec),
    /// An action by its identity: a module's (`edit.<id>`) or a host `mask.*` command, which the
    /// registry resolves through its one action lookup ([`crate::ActionRef`]) and the editor runs
    /// through its one action path.
    Action(String),
    /// A read-only query by its identity: a module's (`query.<id>`) or one of the host's own reads
    /// (`mask.list`, `mask.sample-input`). It writes nothing, so it never emits an event.
    Query(String),
    /// A module's worker task by its identity: `task.<id>` names task `<id>`. The request queues a
    /// capability job, so it carries the `request` envelope and a retry returns the first job; the
    /// catalog owner answers it and announces the task when it succeeds.
    Task(String),
}

/// Who answers a resolved method.
pub(super) enum Route<'m> {
    /// The editor service, with the caller's session: [`Method::plan`].
    Service,
    Owner(OwnerHandler),
    /// The capability host, which queues the task this identity names.
    Task(&'m str),
}

impl Method {
    /// The mutation envelope the method carries. An action — a module's or a mask command — changes
    /// an asset, which has a revision; a task queues a job, which has none.
    pub(super) fn envelope(&self) -> Envelope {
        match self {
            Self::Host(spec) => spec.params.envelope,
            Self::Action(_) => Envelope::Revision,
            Self::Task(_) => Envelope::Request,
            Self::Query(_) => Envelope::None,
        }
    }

    pub(super) fn mutates(&self) -> bool {
        self.envelope() != Envelope::None
    }

    /// Who answers a retry of the method: what a host method's entry declares. An action — a
    /// module's or a mask command — changes an asset, whose catalog request log answers it; a task
    /// queues a job, which the owner's request table answers.
    pub(super) fn retries(&self) -> Retries {
        match self {
            Self::Host(spec) => spec.retries,
            Self::Action(_) => Retries::Catalog,
            Self::Task(_) => Retries::Owner,
            Self::Query(_) => Retries::None,
        }
    }

    pub(super) fn route(&self) -> Route<'_> {
        match self {
            Self::Host(MethodSpec {
                handler: Handler::Owner(handler),
                ..
            }) => Route::Owner(*handler),
            Self::Task(task_id) => Route::Task(task_id),
            Self::Host(_) | Self::Action(_) | Self::Query(_) => Route::Service,
        }
    }

    /// Answer a method the editor service answers, evaluating a planned sample here, for the tests
    /// that serve a method against a service and a session directly. The owner plans instead
    /// ([`Method::plan`]) and never sends it one of its own, which [`Method::route`] names.
    #[cfg(test)]
    pub(super) fn serve(
        &self,
        service: &mut EditorService,
        session: &mut ClientSession,
        params: &Value,
    ) -> Result<Value, Error> {
        self.plan(service, session, params)?.0.answer()
    }

    /// Plan a method the editor service answers: its value, or a sample for the caller to
    /// evaluate where it chooses, and what its handler reports it changed. The catalog owner calls
    /// this, so a sample through a spatial layer never runs on its thread.
    pub(super) fn plan(
        &self,
        service: &mut EditorService,
        session: &mut ClientSession,
        params: &Value,
    ) -> Result<(Planned, Changed), Error> {
        let read = |value: Value| (Planned::Value(value), Changed::Nothing);
        let mutated = |mutated: Mutated| (Planned::Value(mutated.value), mutated.changed);
        match self {
            Self::Host(MethodSpec {
                handler: Handler::Planned(handler),
                ..
            }) => handler(service, session, params).map(|planned| (planned, Changed::Nothing)),
            Self::Host(MethodSpec {
                handler: Handler::Service(handler),
                ..
            }) => handler(service, session, params).map(read),
            Self::Host(MethodSpec {
                handler: Handler::Mutating(handler),
                ..
            }) => handler(service, session, params).map(mutated),
            Self::Action(action_id) => {
                edit_action(service, session, action_id, params).map(mutated)
            }
            Self::Query(query_id) => module_query(service, session, query_id, params).map(read),
            Self::Host(MethodSpec {
                name,
                handler: Handler::Owner(_),
                ..
            }) => Err(owner_answered(name)),
            Self::Task(_) => Err(owner_answered("a task")),
        }
    }
}

fn owner_answered(name: &str) -> Error {
    Error::internal(format!("{name} is answered by the catalog owner"))
}

/// Task method names are generated in a third namespace: task `generate-proof-tint` is
/// `task.generate-proof-tint`.
pub(super) fn task_method(task_id: &str) -> String {
    format!("{TASK_PREFIX}{task_id}")
}

pub(super) fn find(service: &EditorService, name: &str) -> Option<Method> {
    if let Some(spec) = METHODS.iter().find(|spec| spec.name == name) {
        return Some(Method::Host(spec));
    }
    let registry = service.registry();
    if let Some(task_id) = name.strip_prefix(TASK_PREFIX) {
        return registry
            .task(task_id)
            .map(|(_, task)| Method::Task(task.id.clone()));
    }
    if let Some(action) = registry.action_for_method(name) {
        return Some(Method::Action(action.descriptor().id.clone()));
    }
    registry
        .query_for_method(name)
        .map(|query| Method::Query(query.descriptor().id.clone()))
}

/// The envelope of a host method by name, for test clients that fill it in.
#[cfg(test)]
pub(crate) fn host_envelope(name: &str) -> Envelope {
    METHODS
        .iter()
        .find(|spec| spec.name == name)
        .map_or(Envelope::None, |spec| spec.params.envelope)
}

/// One method's entry in `schema.list`: the one shape every method is listed in, whether the host,
/// a module's action, query or task, or a mask command declares it. Whether it mutates and the
/// envelope it names are [`Method::envelope`], the envelope dispatch requires. `required` and
/// `optional` are the fields the method has beside its `declared` parameters, such as its envelope;
/// every method lists its declared parameters, typed with the one parameter vocabulary, and an
/// action or a mask command also lists whether it is a `patch`.
///
/// A declared parameter is required exactly when the descriptor declares it required, it carries no
/// default and the method is not a patch or the parameter is an identity: a patch carries whichever
/// fields the caller names, but the identities it addresses say which state those fields merge over,
/// and a default is what a client seeds or resets the field to. Every other one is optional.
fn method_schema(
    method: &Method,
    mut required: Vec<Value>,
    mut optional: Map<String, Value>,
    notes: &str,
    declared: Option<&[ParameterDescriptor]>,
    patch: Option<bool>,
) -> Value {
    for parameter in declared.unwrap_or_default() {
        if parameter.required
            && parameter.default.is_none()
            && (patch != Some(true) || parameter.kind.is_identity())
        {
            required.push(json!(parameter.name));
        } else {
            optional.insert(parameter.name.clone(), json!(parameter.notes));
        }
    }
    let envelope = method.envelope();
    let mut schema = json!({
        "mutates": method.mutates(),
        "required": required,
        "optional": optional,
        "notes": notes,
    });
    if let Some(parameters) = declared {
        schema["parameters"] = json!(parameters);
    }
    if let Some(name) = envelope.name() {
        schema["mutation"] = json!(name);
    }
    if let Some(patch) = patch {
        schema["patch"] = json!(patch);
    }
    schema
}

/// The source kinds a module's generated methods apply to, listed only when the module does not
/// apply to every kind — as an effect's `sources` is — so a method of a module that applies
/// everywhere is listed exactly as before kinds were declared.
fn method_sources(descriptor: &crate::ModuleDescriptor) -> Option<Value> {
    let kinds: Vec<crate::SourceTag> = crate::SourceTag::ALL
        .into_iter()
        .filter(|kind| descriptor.applies_to(*kind))
        .collect();
    (kinds.len() != crate::SourceTag::ALL.len()).then(|| json!(kinds))
}

/// Mark each declared parameter of `action` that a control variant supersedes with `superseded:
/// [{source, by}]`: on the global target of a photo of `source` the field is refused and `by` —
/// `set-raw.temperature` — is its one path, so a client learns the refusal without trying it.
fn mark_superseded(schema: &mut Value, action: &str, superseded: &[crate::Superseded<'_>]) {
    let Some(parameters) = schema.get_mut("parameters").and_then(Value::as_array_mut) else {
        return;
    };
    for parameter in parameters {
        let Some(name) = parameter.get("name").and_then(Value::as_str) else {
            continue;
        };
        let by: Vec<Value> = superseded
            .iter()
            .filter(|field| field.action == action && field.parameter == name)
            .map(|field| json!({"source": field.source, "by": field.by()}))
            .collect();
        if !by.is_empty() {
            parameter["superseded"] = Value::Array(by);
        }
    }
}

pub fn schemas(registry: &ModuleRegistry) -> Value {
    // The host's own methods, from the parameters each one declares, listed as a generated
    // method's are: the envelope's `mutation` field is required and described once, below.
    let mut methods: Map<String, Value> = METHODS
        .iter()
        .map(|spec| {
            let required = spec
                .params
                .envelope
                .name()
                .map(|_| json!("mutation"))
                .into_iter()
                .collect();
            let schema = method_schema(
                &Method::Host(spec),
                required,
                Map::new(),
                spec.notes,
                Some((spec.params.parameters)()),
                None,
            );
            (spec.name.to_owned(), schema)
        })
        .collect();
    let descriptors = registry.descriptors();
    let host = registry.host_descriptors();
    // The fields a control variant supersedes, derived once from the variants.
    let superseded = registry.superseded();
    // The modules' descriptors and the host's own — the `mask.*` family — through one loop, so a
    // client discovers a mask command and a module action from one listing in one shape. Each
    // method's name is the one its action or query is called through ([`crate::ActionRef::method`]).
    for descriptor in descriptors.iter().copied().chain(host) {
        // One maskable effect makes this module's actions carry the target field; the registry
        // answers the same question the same way for dispatch. The `mask` field is the host's and
        // no module parses it, so it is listed with the envelope rather than among the module's
        // declared parameters, and declared on its own as `target`, an identity parameter checked
        // like any other: sending it to any other action is a validation error naming that action.
        let maskable = descriptor.effects.iter().any(|effect| effect.maskable);
        let sources = method_sources(descriptor);
        for action in &descriptor.actions {
            let Some(resolved) = registry.resolve_action(&action.id) else {
                continue;
            };
            let target = crate::editor::mask_target_parameter();
            let mut optional = Map::new();
            if maskable {
                optional.insert(target.name.clone(), json!(target.notes));
            }
            let mut schema = method_schema(
                &Method::Action(action.id.clone()),
                vec![json!("asset_id"), json!("mutation")],
                optional,
                &action.notes,
                Some(&action.parameters),
                Some(action.patch),
            );
            if maskable {
                schema["target"] = json!(target);
            }
            if let Some(sources) = &sources {
                schema["sources"] = sources.clone();
            }
            mark_superseded(&mut schema, &action.id, &superseded);
            methods.insert(resolved.method(), schema);
        }
        // A query reads the stack of `entry_id`, the session's selection by default.
        for query in &descriptor.queries {
            let Some(resolved) = registry.resolve_query(&query.id) else {
                continue;
            };
            let mut optional = Map::new();
            optional.insert(
                "entry_id".to_owned(),
                json!("entry to ask about; default the session's selection"),
            );
            // A maskable module's query asks about the target its actions would edit.
            let target = crate::editor::mask_target_parameter();
            if maskable && matches!(resolved, crate::QueryRef::Module(..)) {
                optional.insert(
                    target.name.clone(),
                    json!(
                        "the mask whose stack to ask about, as this module's actions take it; \
                         omit it to ask about the layer that applies everywhere"
                    ),
                );
            }
            let mut schema = method_schema(
                &Method::Query(query.id.clone()),
                vec![json!("asset_id")],
                optional,
                &query.notes,
                Some(&query.parameters),
                None,
            );
            if maskable && matches!(resolved, crate::QueryRef::Module(..)) {
                schema["target"] = json!(target);
            }
            if let Some(sources) = &sources {
                schema["sources"] = sources.clone();
            }
            methods.insert(resolved.method(), schema);
        }
        // A task carries the `request` envelope, and `asset_id` and `profile_id` are its envelope
        // when it declares them. The request queues a task job and answers `{job_id, status}`; a
        // retry answers the first job.
        for task in &descriptor.tasks {
            let required = std::iter::once((true, "mutation"))
                .chain([(task.asset, "asset_id"), (task.profile, "profile_id")])
                .filter(|(declared, _)| *declared)
                .map(|(_, field)| json!(field))
                .collect();
            let notes = format!(
                "{} Checks the task's requirements (not-ready with data.requirements) and a live grant for each capability it uses (consent-required naming the first missing one) before anything is queued, then queues a task job on the module lane; returns {{job_id, status, deduplicated}}, and a retry with the same request_id returns the first job and starts none; the job's result, read with job.read, is {{result, artifacts}} when it succeeds",
                task.notes
            );
            let schema = method_schema(
                &Method::Task(task.id.clone()),
                required,
                Map::new(),
                &notes,
                Some(&task.parameters),
                None,
            );
            methods.insert(task_method(&task.id), schema);
        }
    }
    json!({
        "protocol": PROTOCOL,
        "coordinate_space": "Each edit uses integer coordinates in its own input image stage after EXIF orientation. A pixel or colour edit addresses the content stage, the source after EXIF orientation, because the host places both before the quarter-turns, reflections and crop that carry them; a colour edit addresses every pixel of that stage and changes no dimension. A number parameter carries a finite JSON number within its declared range, such as an angle in degrees or a rectangle normalized to its stage; a JSON integer is accepted and passed through unchanged.",
        "methods": methods,
        "modules": descriptors,
        // The host's own descriptors, in the module shape: the `mask.*` family's actions, queries
        // and controls, which `module.list` lists under the same key.
        "host": host,
        // The host's path primitives, described here because a `points` parameter is the one
        // parameter kind whose value a client has to construct rather than move a widget to: a
        // painting action is a canvas gesture, so nothing in the control vocabulary edits a path.
        // Everything needed to post one — the coordinate space and its range, the stored precision,
        // the decimation the desktop applies before it posts, the deviation that leaves, and the
        // per-stroke bound — is published here, so an agent draws the same stored stroke a hand
        // does without reading any desktop code.
        "paths": {
            "coordinates": "A points parameter carries an ordered list of [x, y] positions in the content stage's normalized coordinates, in drawn order, where 1.0 is the stage's height on both axes, so a shape is the shape it looks like at any aspect ratio. Positions are not sorted and may repeat or reverse: a path is a path and not a function.",
            "range": [path::COORDINATE_MIN, path::COORDINATE_MAX],
            "stored_steps_per_unit": path::COORDINATE_STEPS_PER_UNIT,
            "decimation_tolerance_of_radius": path::DECIMATION_TOLERANCE_OF_RADIUS,
            "decimation_tolerance_min": path::DECIMATION_TOLERANCE_MIN,
            "grid_rounding": path::GRID_ROUNDING,
            "posted_points_per_stroke": path::POSTED_POINTS_PER_STROKE,
            "points_per_stroke": path::POINTS_PER_STROKE,
            "notes": "A posted path is snapped to a grid of stored_steps_per_unit steps per unit and decimated on that grid at a tolerance relative to the radius it is drawn with: decimation_tolerance_of_radius times the radius as stored on the same grid, and never less than decimation_tolerance_min. A stored position is therefore at most that tolerance plus grid_rounding from the position that was posted, and at one radius the same posted path always produces the same stored stroke. Decimation is idempotent: a desktop decimates at its radius before it posts, and a path posted undecimated arrives at the same stored bytes. A posted path holds at most posted_points_per_stroke positions, checked before decimation, and the stroke it decimates to at most points_per_stroke, checked after. A stroke is stored once under the hash of its contents and an entry's recipe references it by that hash, so a payload's strokes field holds addresses and never positions.",
        },
        // The one path bound that is the mask's own rather than the host path primitive's, so the
        // `paths` block above says nothing about masks and this one says what a mask adds. The mask
        // commands' widgets are the host descriptor's `controls`.
        "masks": {
            "points_per_mask": crate::POINTS_PER_MASK,
        },
        // Every mutating method names its envelope in its own `mutation` field.
        "mutation": {
            "revision": Envelope::Revision.fields(),
            "request": Envelope::Request.fields(),
            "notes": "Every mutating method carries a mutation envelope. A method that changes an asset or a module's settings, which have a revision, carries revision: expected_revision must be that revision or the request is a conflict. Every other mutating method carries request. request_id and actor are 1..128 characters. A retry with the same request_id and the same input returns the first answer, marked deduplicated: true, and emits no event; the same request_id with different input is a conflict. An asset change's request_id is unique per asset and is remembered durably with the change; every other request_id, a settings write's included, is unique per method family, the method name without its last segment, and is remembered for the owner's lifetime, bounded to the most recent requests, after which a settings write's retry conflicts on its revision.",
        },
    })
}

/// The history page `history.list` and `history.lineage` answer when the request names no `limit`.
const DEFAULT_HISTORY_PAGE: usize = 50;

host_params! {
    pub(super) struct CatalogList {
        limit: Option<usize> = integer(1, MAX_ASSET_PAGE as i64).default(DEFAULT_ASSET_PAGE),
        after: Option<AssetId> = asset().notes("the last asset of the previous page, its next cursor; default the first page"),
    }
}

host_params! {
    pub(super) struct AssetParams {
        asset_id: AssetId = asset(),
    }
}

host_params! {
    pub(super) struct EntryParams {
        asset_id: AssetId = asset(),
        entry_id: EntryId = entry(),
    }
}

host_params! {
    pub(super) struct PreviewSelect {
        asset_id: AssetId = asset(),
        entry_id: EntryId = entry(),
        keep_geometry: Option<bool> = boolean().default(false).notes("show the entry with the geometry layers (orientation, straighten, crop) of the entry the session displays now, so only the adjustments differ; false shows the entry's own geometry"),
    }
}

host_params! {
    pub(super) struct SourceInspect {
        asset_id: AssetId = asset(),
        entry_id: Option<EntryId> = entry().notes("historical entry; default current"),
    }
}

host_params! {
    pub(super) struct HistoryList {
        asset_id: AssetId = asset(),
        before_sequence: Option<u64> = sequence().notes("list the entries before this sequence; default the newest"),
        limit: Option<usize> = integer(1, MAX_HISTORY_PAGE as i64).default(DEFAULT_HISTORY_PAGE),
    }
}

host_params! {
    pub(super) struct HistoryLineage {
        asset_id: AssetId = asset(),
        entry_id: Option<EntryId> = entry().notes("start entry; default current"),
        limit: Option<usize> = integer(1, MAX_HISTORY_PAGE as i64).default(DEFAULT_HISTORY_PAGE),
    }
}

host_params! {
    pub(super) struct RecipeDescribe {
        asset_id: AssetId = asset(),
        entry_id: Option<EntryId> = entry().notes("entry to describe; default current"),
    }
}

host_params! {
    pub(super) struct ModuleList {
        asset_id: Option<AssetId> = asset().notes("keep only the modules that apply to this asset's source kind; default every module"),
    }
}

host_params! {
    pub(super) struct Navigate {
        asset_id: AssetId = asset(),
        mutation: Mutation,
    }
}

host_params! {
    pub(super) struct Restore {
        asset_id: AssetId = asset(),
        mutation: Mutation,
        entry_id: EntryId = entry(),
    }
}

host_params! {
    pub(super) struct VersionCreate {
        asset_id: AssetId = asset(),
        name: String = string(MAX_VERSION_NAME).notes("non-empty after trimming"),
        mutation: MutationRequest,
        entry_id: Option<EntryId> = entry().notes("entry to name; default current"),
    }
}

host_params! {
    pub(super) struct VersionDelete {
        asset_id: AssetId = asset(),
        name: String = string(MAX_VERSION_NAME),
        mutation: MutationRequest,
    }
}

host_params! {
    pub(super) struct PresetParams {
        preset_id: PresetId = preset(),
    }
}

host_params! {
    pub(super) struct PresetCreate {
        name: String = string(MAX_PRESET_NAME).notes("non-empty after trimming"),
        settings: Map<String, Value> = settings(),
        mutation: MutationRequest,
        group: Option<String> = string(MAX_PRESET_GROUP).notes("non-empty after trimming; default User presets"),
    }
}

host_params! {
    pub(super) struct PresetCapture {
        asset_id: AssetId = asset(),
        fields: Map<String, Value> = json("{action: [field, ...] or true}: the fields of each field-patch action to read, true for all of them"),
        entry_id: Option<EntryId> = entry().notes("entry to read; default the session's selection"),
    }
}

host_params! {
    pub(super) struct PresetUpdate {
        preset_id: PresetId = preset(),
        mutation: MutationRequest,
        name: Option<String> = string(MAX_PRESET_NAME).notes("non-empty after trimming"),
        group: Option<String> = string(MAX_PRESET_GROUP).notes("non-empty after trimming"),
        settings: Option<Map<String, Value>> = settings().notes("checked against the registry"),
    }
}

host_params! {
    pub(super) struct PresetDelete {
        preset_id: PresetId = preset(),
        mutation: MutationRequest,
    }
}

host_params! {
    pub(super) struct PresetInspect {
        content: String = text(MAX_PRESET_BYTES).notes("the preset file's text"),
        file_name: Option<String> = name().notes("the file's name, for the fallback preset name and the origin"),
    }
}

host_params! {
    pub(super) struct PresetImport {
        content: String = text(MAX_PRESET_BYTES).notes("the preset file's text"),
        mutation: MutationRequest,
        file_name: Option<String> = name().notes("the file's name, for the fallback preset name and the origin"),
        name: Option<String> = string(MAX_PRESET_NAME).notes("overrides the file's name"),
        group: Option<String> = string(MAX_PRESET_GROUP).notes("overrides the file's group; default Imported"),
    }
}

host_params! {
    pub(super) struct ViewSet {
        zoom: Option<Zoom> = json("{mode: fit} or {mode: percent, value: 10..1600}"),
        pan_x: Option<f32> = number(f32::MIN as f64, f32::MAX as f64),
        pan_y: Option<f32> = number(f32::MIN as f64, f32::MAX as f64),
    }
}

host_params! {
    pub(super) struct WorkspaceSet {
        state_panel: Option<bool> = boolean(),
        tools_panel: Option<bool> = boolean(),
        mode: Option<String> = name().notes("pointer or an available module id that declares a canvas interaction"),
        thirds: Option<bool> = boolean(),
        clip_shadows: Option<bool> = boolean().notes("show the shadow clipping overlay"),
        clip_highlights: Option<bool> = boolean().notes("show the highlight clipping overlay"),
        // Each spelling is declared as an option, so a client reads the vocabulary from the schema
        // and an unknown one is refused with the vocabulary spelled out.
        mask_overlay: Option<String> = enumeration(MaskOverlayMode::ALL.map(MaskOverlayMode::as_str)).notes("what the canvas draws of the selected mask"),
        mask_overlay_colour: Option<String> = enumeration(MaskOverlayColour::ALL.map(MaskOverlayColour::as_str)).notes("the tint the mask overlay is drawn in"),
    }
}

host_params! {
    pub(super) struct DraftBegin {
        asset_id: AssetId = asset(),
        action: String = name().notes("the action the draft edits: a module action or a mask.* gesture"),
        /// The mask and component a `mask.*` gesture edits. A module action's draft takes neither.
        mask: Option<MaskId> = mask().notes("the mask a mask.* gesture edits, or the mask an action of a maskable effect drafts through"),
        component: Option<ComponentId> = component().notes("the component inside that mask, for a mask.* gesture only"),
    }
}

host_params! {
    pub(super) struct DraftSet {
        draft_id: DraftId = draft(),
        fields: Map<String, Value> = json("{field: value}: the action's fields to set, each checked against its declaration"),
    }
}

host_params! {
    pub(super) struct DraftParams {
        draft_id: DraftId = draft(),
    }
}

host_params! {
    pub(super) struct DraftCommit {
        draft_id: DraftId = draft(),
        mutation: Mutation,
    }
}

host_params! {
    pub(super) struct RenderSample {
        asset_id: AssetId = asset(),
        x: u32 = pixel_coordinate(),
        y: u32 = pixel_coordinate(),
        draft_id: Option<DraftId> = draft().notes("this client's draft to sample instead of the stored stack"),
    }
}

host_params! {
    pub(super) struct RenderLocate {
        asset_id: AssetId = asset(),
        x: u32 = pixel_coordinate(),
        y: u32 = pixel_coordinate(),
        entry_id: Option<EntryId> = entry().notes("entry to locate in; default the session's selection"),
    }
}

host_params! {
    pub(super) struct RenderTransform {
        asset_id: AssetId = asset(),
        entry_id: Option<EntryId> = entry().notes("entry to answer for; default the session's selection"),
    }
}

host_params! {
    pub(super) struct ArtifactInspect {
        artifact_id: ArtifactId = artifact(),
    }
}

/// The modules `module.list` answers with: those that apply to a photo of `kind`, or every module
/// when no photo is named. Every module describes the source kinds its effects may exist on, so a
/// caller holding the whole list and filtering it with [`crate::ModuleDescriptor::applies_to`]
/// for a photo's kind holds exactly what naming that photo returns.
fn listed_modules(
    registry: &crate::ModuleRegistry,
    kind: Option<crate::SourceTag>,
) -> Vec<&crate::ModuleDescriptor> {
    registry
        .descriptors()
        .into_iter()
        .filter(|module| kind.is_none_or(|kind| module.applies_to(kind)))
        .collect()
}

fn module_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: ModuleList,
) -> Result<Value, Error> {
    let kind = p
        .asset_id
        .as_ref()
        .map(|id| service.state(id))
        .transpose()?
        .map(|state| state.asset.source.tag());
    let modules = listed_modules(service.registry(), kind);
    // The host's own descriptors beside them, in the same shape, so an agent discovers a `mask.*`
    // command, its parameters and its controls exactly as it discovers a module action.
    Ok(json!({"modules": modules, "host": service.registry().host_descriptors()}))
}

/// Every generated action method, a module's `edit.<id>` and a host `mask.*` command alike:
/// `asset_id` and `mutation` are the envelope, the remaining top-level fields are the action's
/// declared parameters, and the editor's one action path answers.
fn edit_action(
    service: &mut EditorService,
    session: &mut ClientSession,
    action_id: &str,
    request: &Value,
) -> Result<Mutated, Error> {
    let mut parameters = params::generated(request)?;
    let asset_id: AssetId = params::take(&mut parameters, "asset_id")?;
    require_current(session, &asset_id)?;
    let mutation: Mutation = params::take(&mut parameters, "mutation")?;
    let result = service.run_action(&asset_id, mutation, action_id, Value::Object(parameters))?;
    Mutated::asset(&result.mutation, &result)
}

/// Every generated query method, a module's `query.<id>` and the host's `mask.list` and
/// `mask.sample-input` alike: `asset_id` is the envelope, `entry_id` names the stack to ask about
/// and defaults to the session's selection exactly as `render.sample` does, and the remaining
/// top-level fields are the query's declared parameters.
///
/// A query is read-only, so unlike an edit it does not require the session to be on current: a
/// client inspecting a historical entry may ask about that entry. Nothing is committed and no event
/// is emitted, whether the query answers or refuses.
fn module_query(
    service: &mut EditorService,
    session: &mut ClientSession,
    query_id: &str,
    request: &Value,
) -> Result<Value, Error> {
    let mut parameters = params::generated(request)?;
    let asset_id: AssetId = params::take(&mut parameters, "asset_id")?;
    let entry_id = params::take_optional(&mut parameters, "entry_id")?;
    let entry_id = selected_entry(service, session, &asset_id, entry_id)?;
    service.run_query(&asset_id, &entry_id, query_id, Value::Object(parameters))
}

fn history_undo(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: Navigate,
) -> Result<Mutated, Error> {
    require_current(session, &p.asset_id)?;
    let result = service.undo(&p.asset_id, p.mutation)?;
    Mutated::asset(&result, &result)
}

fn history_redo(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: Navigate,
) -> Result<Mutated, Error> {
    require_current(session, &p.asset_id)?;
    let result = service.redo(&p.asset_id, p.mutation)?;
    Mutated::asset(&result, &result)
}

fn history_restore(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: Restore,
) -> Result<Mutated, Error> {
    let result = service.restore(&p.asset_id, p.mutation, &p.entry_id)?;
    // Restoring makes the restored state current, so this asset's preview returns to it; a
    // selection of another asset is left as it is.
    if !session.preview.can_edit(&p.asset_id) {
        session
            .preview
            .select(&p.asset_id, HistorySelection::Current)?;
        session.touch();
    }
    Mutated::asset(&result, &result)
}

fn version_create(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: VersionCreate,
) -> Result<Mutated, Error> {
    let result =
        service.create_version(&p.asset_id, &p.name, p.entry_id.as_ref(), &p.mutation.actor)?;
    Mutated::unrevised(result.outcome, &result)
}

fn version_delete(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: VersionDelete,
) -> Result<Mutated, Error> {
    let result = service.delete_version(&p.asset_id, &p.name)?;
    Mutated::unrevised(result.outcome, &result)
}

fn preset_read(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: PresetParams,
) -> Result<Value, Error> {
    let (record, source_text) = service.preset(&p.preset_id)?;
    let mut preset = value(record)?;
    preset["source_text"] = json!(source_text);
    Ok(json!({"preset": preset}))
}

fn preset_create(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: PresetCreate,
) -> Result<Mutated, Error> {
    let preset =
        service.create_preset(&p.name, p.group.as_deref(), &p.settings, &p.mutation.actor)?;
    Mutated::unrevised(MutationOutcome::Applied, json!({"preset": preset}))
}

/// Capture reads the entry the caller names, or the session's selection exactly as `render.sample`
/// resolves it, so the desktop captures the entry it displays.
fn preset_capture(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: PresetCapture,
) -> Result<Value, Error> {
    let entry_id = selected_entry(service, session, &p.asset_id, p.entry_id)?;
    Ok(json!({"settings": service.capture_preset(&p.asset_id, &entry_id, &p.fields)?}))
}

fn preset_update(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: PresetUpdate,
) -> Result<Mutated, Error> {
    let result = service.update_preset(
        &p.preset_id,
        &p.mutation.actor,
        p.name.as_deref(),
        p.group.as_deref(),
        p.settings.as_ref(),
    )?;
    Mutated::unrevised(result.outcome, &result)
}

fn preset_delete(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: PresetDelete,
) -> Result<Mutated, Error> {
    let outcome = service.delete_preset(&p.preset_id)?;
    Mutated::unrevised(
        outcome,
        json!({"outcome": outcome, "deleted": outcome == MutationOutcome::Applied}),
    )
}

fn preset_import(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: PresetImport,
) -> Result<Mutated, Error> {
    let preset = service.import_preset(
        &p.content,
        p.file_name.as_deref(),
        p.name.as_deref(),
        p.group.as_deref(),
        &p.mutation.actor,
    )?;
    Mutated::unrevised(
        MutationOutcome::Applied,
        json!({"report": preset.report, "preset": preset}),
    )
}

fn preview_select(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: PreviewSelect,
) -> Result<Value, Error> {
    // The current entry is the live state, not a historical snapshot: selecting it is Return to
    // current, so the session keeps following later commits and editing stays enabled.
    if service.current_entry_id(&p.asset_id)? == p.entry_id {
        let generation = session
            .preview
            .select(&p.asset_id, HistorySelection::Current)?;
        session.touch();
        return Ok(json!({"generation": generation, "session": session_value(service, session)?}));
    }
    service.entry(&p.asset_id, &p.entry_id)?;
    // The geometry the session displays of this asset now: the framing it already shows, the
    // selected entry's own, or, at current, the current entry's. Selections are per asset, so a
    // selection of another asset never frames this one.
    let geometry = if p.keep_geometry.unwrap_or(false) {
        let displayed = match session.preview.selections.get(&p.asset_id) {
            Some(selected) => selected
                .geometry_from
                .clone()
                .unwrap_or_else(|| selected.entry_id.clone()),
            None => service.current_entry_id(&p.asset_id)?,
        };
        // An entry framed by itself is just that entry.
        Some(displayed).filter(|displayed| displayed != &p.entry_id)
    } else {
        None
    };
    let generation = session.preview.select_framed(
        &p.asset_id,
        HistorySelection::Entry(p.entry_id),
        geometry,
    )?;
    session.touch();
    Ok(json!({"generation": generation, "session": session_value(service, session)?}))
}

fn preview_return_current(
    service: &mut EditorService,
    session: &mut ClientSession,
    _: NoParams,
) -> Result<Value, Error> {
    let generation = session.preview.return_current();
    session.touch();
    Ok(json!({"generation": generation, "session": session_value(service, session)?}))
}

fn view_set(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: ViewSet,
) -> Result<Value, Error> {
    if let Some(zoom) = p.zoom {
        session.preview.view.set_zoom(zoom)?;
    }
    if p.pan_x.is_some() || p.pan_y.is_some() {
        session.preview.view.pan_to(
            p.pan_x.unwrap_or(session.preview.view.pan_x),
            p.pan_y.unwrap_or(session.preview.view.pan_y),
        )?;
    }
    session.touch();
    session_value(service, session)
}

/// The canvas modes this registry offers: the pointer plus every available module that declares a
/// canvas interaction. `O(modules)`; it touches no image resource.
fn canvas_modes(registry: &ModuleRegistry) -> Vec<String> {
    // The host modes first: the pointer, the mask mode — a mask is a host object rather than a
    // module — and one per canvas pick the host itself declares, which is one per sampling component
    // kind. A pick's mode is its own action's method name, read from the same table that generates
    // the command, so registering a kind is what makes its mode legal here too. Then one per module
    // that declares a canvas.
    let mut modes = vec![POINTER_MODE.to_owned(), MASK_MODE.to_owned()];
    modes.extend(
        crate::mask::commands::canvas()
            .iter()
            .filter_map(|pick| match pick {
                crate::CanvasInteraction::SampleApply { action, .. } => Some(action.clone()),
                crate::CanvasInteraction::PointPick { .. }
                | crate::CanvasInteraction::CropFrame { .. } => None,
            }),
    );
    modes.extend(
        registry
            .descriptors()
            .iter()
            .filter(|descriptor| descriptor.is_available() && descriptor.canvas.is_some())
            .map(|descriptor| descriptor.id.clone()),
    );
    modes
}

fn workspace_set(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: WorkspaceSet,
) -> Result<Value, Error> {
    // Validate before changing anything, so a rejected request leaves the session as it was.
    if let Some(mode) = &p.mode {
        let modes = canvas_modes(service.registry());
        if !modes.iter().any(|accepted| accepted == mode) {
            return Err(Error::validation(format!(
                "mode must be one of {}",
                modes.join(", ")
            )));
        }
    }
    let mask_overlay = match &p.mask_overlay {
        None => None,
        Some(value) => Some(MaskOverlayMode::parse(value).ok_or_else(|| {
            Error::validation(format!(
                "mask_overlay must be one of {}",
                MaskOverlayMode::ALL.map(MaskOverlayMode::as_str).join(", ")
            ))
        })?),
    };
    let mask_overlay_colour = match &p.mask_overlay_colour {
        None => None,
        Some(value) => Some(MaskOverlayColour::parse(value).ok_or_else(|| {
            Error::validation(format!(
                "mask_overlay_colour must be one of {}",
                MaskOverlayColour::ALL
                    .map(MaskOverlayColour::as_str)
                    .join(", ")
            ))
        })?),
    };
    if let Some(mode) = p.mode {
        session.workspace.mode = mode;
    }
    if let Some(state_panel) = p.state_panel {
        session.workspace.state_panel = state_panel;
    }
    if let Some(tools_panel) = p.tools_panel {
        session.workspace.tools_panel = tools_panel;
    }
    if let Some(thirds) = p.thirds {
        session.workspace.thirds = thirds;
    }
    // Overlay settings are per-client view state: they change no raster, recipe or histogram.
    if let Some(clip_shadows) = p.clip_shadows {
        session.workspace.clip_shadows = clip_shadows;
    }
    if let Some(clip_highlights) = p.clip_highlights {
        session.workspace.clip_highlights = clip_highlights;
    }
    // The mask overlay is the same kind of view state: it chooses what the canvas draws of the
    // selected mask and commits nothing.
    if let Some(mode) = mask_overlay {
        session.workspace.mask_overlay = mode;
    }
    if let Some(colour) = mask_overlay_colour {
        session.workspace.mask_overlay_colour = colour;
    }
    session.touch();
    session_value(service, session)
}

/// Plan one pixel against the stack the caller names, reading the session and the catalog now. A
/// point that costs `O(layers)` is answered here; one through a spatial layer is returned planned,
/// for the catalog owner to hand to its point worker.
fn render_sample(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: RenderSample,
) -> Result<Planned, Error> {
    let plan = match &p.draft_id {
        // A draft's effective recipe answers the point, so a readout during a gesture matches the
        // frame the same draft is previewing.
        Some(draft_id) => {
            let draft = session.held_draft(draft_id)?;
            service.point_draft(&p.asset_id, draft, p.x, p.y)?
        }
        None => {
            let entry_id = selected_entry(service, session, &p.asset_id, None)?;
            let framing = session_framing(session, &p.asset_id, None);
            service.point_selected(
                &p.asset_id,
                AnalysisSelection::framed(&entry_id, framing.as_ref()),
                p.x,
                p.y,
            )?
        }
    };
    if plan.evaluates_spatial() {
        Ok(Planned::Sample(Box::new(plan)))
    } else {
        sample_value(plan.evaluate()?).map(Planned::Value)
    }
}

fn sample_value(sample: PixelSample) -> Result<Value, Error> {
    let mut sampled = value(sample)?;
    sampled["source_detail_ready"] = json!(true);
    Ok(sampled)
}

/// `conflicted` is derived, never notified: the asset moved under the draft. Every read, set,
/// commit and session report recomputes it from the asset's current revision.
fn refresh_conflict(service: &EditorService, session: &mut ClientSession) -> Result<(), Error> {
    if let Some(draft) = &mut session.draft {
        draft.conflicted = draft.base_revision != service.revision(&draft.asset_id)?;
    }
    Ok(())
}

/// The session as a client reads it, with its draft's conflict state recomputed first.
fn session_value(service: &EditorService, session: &mut ClientSession) -> Result<Value, Error> {
    refresh_conflict(service, session)?;
    value(session)
}

/// The action a draft will run, and the parameters its fields are validated against.
///
/// Resolved through the registry's one action lookup, so a host `mask.*` command resolves here as a
/// module action does: `draft.set`, `draft.read` and `draft.reapply` validate a drafted gesture's
/// fields against the same declared parameters with the same generic check, and the conflict,
/// Discard and Reapply rules below are the delivered ones rather than a second copy.
fn draft_action<'a>(service: &'a EditorService, action_id: &str) -> Result<ActionRef<'a>, Error> {
    service
        .registry()
        .resolve_action(action_id)
        .ok_or_else(|| Error::validation(format!("unknown action {action_id}")))
}

fn draft_begin(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: DraftBegin,
) -> Result<Value, Error> {
    if let Some(draft) = &session.draft {
        return Err(Error::conflict(format!(
            "this client already holds draft {} of action {}",
            draft.draft_id, draft.action
        )));
    }
    if !session.preview.can_edit(&p.asset_id) {
        return Err(Error::validation(
            "return to current before drafting an edit",
        ));
    }
    // A gesture's target is the objects it edits, fixed when it begins and sent with its commit as
    // the request fields they are: a `mask.*` gesture's mask and component, or the host's one
    // `mask` field of a maskable module action, so a masked slider previews what it is about to
    // commit instead of committing blind. It is checked by the checks its commit runs, and a draft
    // its commit would refuse for what the photo is — a RAW development on a JPEG — is refused
    // here, in the commit's words, rather than at its first preview.
    let target = [
        ("mask", p.mask.map(|mask| mask.as_str().to_owned())),
        (
            "component",
            p.component.map(|component| component.as_str().to_owned()),
        ),
    ]
    .into_iter()
    .filter_map(|(name, identity)| identity.map(|identity| (name.to_owned(), identity)))
    .collect();
    let target = service.draft_target(&p.asset_id, &p.action, target)?;
    let revision = service.revision(&p.asset_id)?;
    let mut draft = crate::Draft::new(&p.action, p.asset_id, revision);
    draft.target = target;
    session.draft = Some(draft);
    session.touch();
    value(session.draft.as_ref().expect("the draft just opened"))
}

fn draft_set(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: DraftSet,
) -> Result<Value, Error> {
    let draft = session.held_draft(&p.draft_id)?;
    // Validate every field before merging any, so a rejected request leaves the draft as it was.
    let action = draft_action(service, &draft.action)?;
    draft.checked_fields(&action.descriptor().parameters, &p.fields)?;
    // So is a field its commit would refuse on this photo's target: Basic's Temperature on a RAW
    // photo's global target, whose one path is the source development's.
    let mask = draft
        .target
        .get(crate::MASK_FIELD)
        .map(|mask| MaskId::parse(mask.as_str()))
        .transpose()?;
    service.check_draft(&draft.asset_id, &draft.action, mask.as_ref(), &p.fields)?;
    let draft = session.draft.as_mut().expect("the draft was just found");
    draft.merge(p.fields);
    session.touch();
    draft_value(service, session)
}

fn draft_read(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: DraftParams,
) -> Result<Value, Error> {
    session.held_draft(&p.draft_id)?;
    draft_value(service, session)
}

fn draft_cancel(
    _: &mut EditorService,
    session: &mut ClientSession,
    p: DraftParams,
) -> Result<Value, Error> {
    session.held_draft(&p.draft_id)?;
    session.draft = None;
    session.touch();
    Ok(json!({"cancelled": true}))
}

fn draft_commit(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: DraftCommit,
) -> Result<Mutated, Error> {
    refresh_conflict(service, session)?;
    let draft = session.held_draft(&p.draft_id)?;
    if draft.conflicted {
        return Err(Error::conflict(
            "the asset changed under this draft; discard it or reapply it",
        ));
    }
    if p.mutation.expected_revision != draft.base_revision {
        return Err(Error::conflict(format!(
            "stale revision {}; this draft is based on revision {}",
            p.mutation.expected_revision, draft.base_revision
        )));
    }
    let asset_id = draft.asset_id.clone();
    let action = draft.action.clone();
    // The drafted fields with the target's over them: exactly the request an independent client
    // would send for the same edit — a module action's host `mask` field, a mask command's declared
    // identities — committed through the one action path the draft's preview planned through. A
    // failed commit keeps the draft, so the client can correct it and try again; a no-op ends it
    // exactly like an applied one, because the gesture is over either way.
    let request = draft.request();
    let result = service.run_action(&asset_id, p.mutation, &action, Value::Object(request))?;
    session.draft = None;
    session.touch();
    Mutated::asset(&result.mutation, &result)
}

fn draft_reapply(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: DraftParams,
) -> Result<Value, Error> {
    let draft = session.held_draft(&p.draft_id)?;
    // Only the fields this client set survive, revalidated against the action they belong to;
    // whatever another client changed meanwhile stays in the layer the commit merges over.
    let action = draft_action(service, &draft.action)?;
    draft.checked_fields(&action.descriptor().parameters, &draft.fields)?;
    let revision = service.revision(&draft.asset_id)?;
    let draft = session.draft.as_mut().expect("the draft was just found");
    draft.base_revision = revision;
    draft.conflicted = false;
    session.touch();
    value(session.draft.as_ref().expect("the draft is still open"))
}

/// The open draft with its conflict state recomputed.
fn draft_value(service: &EditorService, session: &mut ClientSession) -> Result<Value, Error> {
    refresh_conflict(service, session)?;
    value(session.draft.as_ref().expect("the draft is still open"))
}

/// Where a view point lands in the content stage. Read-only: the session's selection decides which
/// entry answers when the caller names none, and nothing is committed or touched.
fn render_locate(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: RenderLocate,
) -> Result<Value, Error> {
    let framing = session_framing(session, &p.asset_id, p.entry_id.as_ref());
    let entry_id = selected_entry(service, session, &p.asset_id, p.entry_id)?;
    value(service.locate_selected(
        &p.asset_id,
        AnalysisSelection::framed(&entry_id, framing.as_ref()),
        p.x,
        p.y,
    )?)
}

/// The geometry tail of one entry as a single affine map. Read-only in every sense: it resolves the
/// entry exactly as `render.locate` does, compiles the stack, composes the tail and touches nothing —
/// no history entry, no event, no session state. A gesture asks once and maps pointer positions
/// itself, which is the whole reason it is a matrix and not a point query.
fn render_transform(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: RenderTransform,
) -> Result<Value, Error> {
    let framing = session_framing(session, &p.asset_id, p.entry_id.as_ref());
    let entry_id = selected_entry(service, session, &p.asset_id, p.entry_id)?;
    value(service.transform_selected(
        &p.asset_id,
        AnalysisSelection::framed(&entry_id, framing.as_ref()),
    )?)
}

/// The entry a read-only question about `asset_id` is answered against: the one the caller named,
/// or the session's selection of that asset — its current entry when the session previews none of
/// its history, whatever it previews of another asset. Every method that reads "the entry the
/// client is looking at" resolves it here, so a client previewing a historical entry asks about the
/// stack it is looking at.
fn selected_entry(
    service: &EditorService,
    session: &ClientSession,
    asset_id: &AssetId,
    named: Option<EntryId>,
) -> Result<EntryId, Error> {
    match named.or_else(|| session.preview.selected_entry(asset_id).cloned()) {
        Some(entry_id) => Ok(entry_id),
        None => service.current_entry_id(asset_id),
    }
}

/// The geometry that frames the session's selection of `asset_id` when the caller names no entry: a
/// client comparing a framed selection asks about the stack it is looking at. A named entry answers
/// for its own stack.
fn session_framing(
    session: &ClientSession,
    asset_id: &AssetId,
    named: Option<&EntryId>,
) -> Option<EntryId> {
    match named {
        Some(_) => None,
        None => session.preview.geometry_from(asset_id).cloned(),
    }
}

/// Refuse an edit to `asset_id` while this session previews one of its historical entries. A
/// selection of another asset never pauses this one.
fn require_current(session: &ClientSession, asset_id: &AssetId) -> Result<(), Error> {
    if session.preview.can_edit(asset_id) {
        Ok(())
    } else {
        Err(Error::conflict(
            "return to current or restore the selected history entry before editing",
        ))
    }
}

pub(super) fn value(value: impl Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|error| Error::internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiResponse;
    use crate::{
        ActionInput, ActionPlan, Availability, EFFECT_FORMAT, EffectDescriptor, EffectStage,
        ExactGeometry, ModuleDescriptor, ParameterDescriptor, Processing, Stage, StageContext,
        ToolModule,
        editor::mutation_json,
        modules::{PATCH_ACTION, PATCH_MODULE, PatchModule},
    };
    use std::{
        collections::HashSet,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
    };

    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
    }

    /// Answer one service method against a service and a session the test holds, exactly as the
    /// owner calls it. The owner's own methods are exercised through an `OwnerHandle` in its tests.
    fn call(
        service: &mut EditorService,
        session: &mut ClientSession,
        method: &str,
        params: Value,
    ) -> ApiResponse {
        let result = match find(service, method) {
            Some(resolved) => {
                assert!(
                    matches!(resolved.route(), Route::Service),
                    "{method} is the owner's"
                );
                resolved.serve(service, session, &params)
            }
            None => Err(Error::protocol(format!("unknown method {method}"))),
        };
        match result {
            Ok(result) => ApiResponse::success(method.into(), 0, result),
            Err(error) => ApiResponse::failure(method.into(), 0, error),
        }
    }

    fn ok(
        service: &mut EditorService,
        session: &mut ClientSession,
        method: &str,
        params: Value,
    ) -> Value {
        let response = call(service, session, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    /// A mutating handler reports a change only when it made one: an asset's no-op and a retry its
    /// catalog log answered changed nothing, an applied or navigated change names the revision it
    /// left, and a change without a revision changed something unless it was a no-op. The owner
    /// announces from this alone; nothing reads the answer's `outcome`, `deduplicated` or
    /// `revision` back.
    #[test]
    fn a_mutating_handler_reports_what_it_changed_and_at_which_revision() {
        let result = |outcome, deduplicated| MutationResult {
            outcome,
            revision: 7,
            current_entry_id: EntryId::new(),
            created_entry_id: None,
            deduplicated,
        };
        for (outcome, deduplicated, changed) in [
            (
                MutationOutcome::Applied,
                false,
                Changed::Something { revision: Some(7) },
            ),
            (
                MutationOutcome::Navigated,
                false,
                Changed::Something { revision: Some(7) },
            ),
            (MutationOutcome::NoOp, false, Changed::Nothing),
            (MutationOutcome::Applied, true, Changed::Nothing),
            (MutationOutcome::NoOp, true, Changed::Nothing),
        ] {
            let result = result(outcome, deduplicated);
            let mutated = Mutated::asset(&result, &result).unwrap();
            assert_eq!(mutated.changed, changed, "{outcome:?} {deduplicated}");
            assert_eq!(
                mutated.value,
                value(&result).unwrap(),
                "the answer is unchanged"
            );
        }
        for (outcome, changed) in [
            (
                MutationOutcome::Applied,
                Changed::Something { revision: None },
            ),
            (MutationOutcome::NoOp, Changed::Nothing),
        ] {
            let mutated = Mutated::unrevised(outcome, json!({"outcome": outcome})).unwrap();
            assert_eq!(mutated.changed, changed, "{outcome:?}");
        }
    }

    /// Answer one mutating service method as [`ok`] does, with what its handler reported changing.
    fn mutated(
        service: &mut EditorService,
        session: &mut ClientSession,
        method: &str,
        params: Value,
    ) -> (Value, Changed) {
        let resolved = find(service, method).expect("a method");
        let (planned, changed) = resolved
            .plan(service, session, &params)
            .unwrap_or_else(|error| panic!("{method}: {error:?}"));
        (planned.answer().expect("an answer"), changed)
    }

    /// `schema.list` checked for what holds of every method rather than what any one publishes:
    /// each name is listed once, every listed method dispatches through the one table as the kind
    /// of method it is, and a method mutates exactly when it names an envelope the schema describes.
    /// What the built-in modules and the host publish, name by name and field by field, is the
    /// committed descriptor snapshot's (`tests/modules/descriptors.rs`).
    #[test]
    fn host_and_generated_methods_are_unique_complete_and_match_the_schema() {
        let catalog =
            std::env::temp_dir().join(format!("luxforge-methods-{}.sqlite", std::process::id()));
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let mut session = ClientSession::default();
        let names: HashSet<&str> = METHODS.iter().map(|spec| spec.name).collect();
        assert_eq!(names.len(), METHODS.len(), "duplicate method names");
        // Every action and query the modules and the host declare, as (method, identity), resolved
        // through the registry's one lookup.
        let registry = Arc::clone(service.registry());
        let (mut actions, mut queries) = (Vec::new(), Vec::new());
        for descriptor in registry
            .descriptors()
            .into_iter()
            .chain(registry.host_descriptors())
        {
            actions.extend(descriptor.actions.iter().map(|action| {
                let method = registry.resolve_action(&action.id).unwrap().method();
                (method, action.id.clone())
            }));
            queries.extend(descriptor.queries.iter().map(|query| {
                let method = registry.resolve_query(&query.id).unwrap().method();
                (method, query.id.clone())
            }));
        }
        let schema = schemas(&registry);
        let listed = schema["methods"].as_object().unwrap();
        // The map holds each name once, so a generated name that collided with another would
        // shorten it.
        assert_eq!(
            listed.len(),
            METHODS.len() + actions.len() + queries.len(),
            "every host, action and query method is listed under a name of its own"
        );
        assert_eq!(
            schema["modules"].as_array().unwrap().len(),
            registry.descriptors().len()
        );
        assert_eq!(
            schema["host"].as_array().unwrap().len(),
            registry.host_descriptors().len()
        );
        for (method, id) in &actions {
            assert!(
                matches!(find(&service, method), Some(Method::Action(found)) if &found == id),
                "{method} does not dispatch as the action {id}"
            );
            assert_eq!(
                listed[method]["mutation"],
                json!("revision"),
                "{method}: every generated action commits a revisioned change"
            );
            // An action is reached only through its own namespace: a host action not as a module's
            // `edit.*`, and a module's not by its bare identity.
            for alias in [id.clone(), format!("edit.{id}")] {
                if &alias != method {
                    assert!(find(&service, &alias).is_none(), "{alias} reaches {id}");
                }
            }
        }
        for (method, id) in &queries {
            assert!(
                matches!(find(&service, method), Some(Method::Query(found)) if &found == id),
                "{method} does not dispatch as the query {id}"
            );
            assert_eq!(listed[method]["mutates"], json!(false), "{method}");
        }
        assert!(find(&service, "edit.missing").is_none());
        // Each host method's schema is generated from the struct its handler parses, so the lists
        // are the parser's own; the owner's generated test sends every method its declared fields
        // and a value of each declared kind in and out of range. Every field is typed with the
        // module vocabulary, as a sound declaration, and listed required or optional exactly as it
        // is declared.
        for spec in METHODS {
            let schema = &listed[spec.name];
            let parameters = (spec.params.parameters)();
            assert_eq!(
                schema["parameters"],
                json!(parameters),
                "{} lists its typed parameters",
                spec.name
            );
            let mut names = HashSet::new();
            for parameter in parameters {
                crate::modules::check_declaration(parameter)
                    .unwrap_or_else(|error| panic!("{}: {error:?}", spec.name));
                assert!(names.insert(parameter.name.as_str()), "{}", spec.name);
            }
            let envelope = spec.params.envelope.name().map(|_| "mutation");
            let required: Vec<&str> = envelope
                .into_iter()
                .chain(
                    parameters
                        .iter()
                        .filter(|parameter| parameter.required)
                        .map(|parameter| parameter.name.as_str()),
                )
                .collect();
            assert_eq!(schema["required"], json!(required), "{}", spec.name);
            let optional: Vec<&str> = parameters
                .iter()
                .filter(|parameter| !parameter.required)
                .map(|parameter| parameter.name.as_str())
                .collect();
            let mut listed_optional: Vec<&str> = schema["optional"]
                .as_object()
                .expect("the optional fields")
                .keys()
                .map(String::as_str)
                .collect();
            listed_optional.sort_unstable();
            let mut optional = optional;
            optional.sort_unstable();
            assert_eq!(listed_optional, optional, "{}", spec.name);
            assert_eq!(
                schema.get("mutation").cloned(),
                spec.params.envelope.name().map(|name| json!(name)),
                "{}",
                spec.name
            );
            assert_eq!(
                envelope.is_some(),
                Method::Host(spec).mutates(),
                "{} carries its envelope as the required mutation field",
                spec.name
            );
        }
        // The envelopes the schema describes are the fields their structs parse.
        let keys = |value: &Value| -> Vec<String> {
            let mut keys: Vec<String> = match value {
                Value::Object(object) => object.keys().cloned().collect(),
                Value::Array(fields) => fields
                    .iter()
                    .map(|field| field.as_str().expect("a field name").to_owned())
                    .collect(),
                other => panic!("not an envelope: {other}"),
            };
            keys.sort();
            keys
        };
        assert_eq!(
            keys(&schema["mutation"]["revision"]),
            keys(&mutation_json(0, "revision"))
        );
        assert_eq!(
            keys(&schema["mutation"]["request"]),
            keys(
                &serde_json::to_value(crate::MutationRequest {
                    request_id: "request".into(),
                    actor: "test".into(),
                })
                .unwrap()
            )
        );
        for (name, method) in listed {
            let resolved = find(&service, name).expect("every listed method resolves");
            // A method mutates exactly when it names an envelope, and the schema describes it.
            let envelope = method.get("mutation");
            assert_eq!(method["mutates"], json!(envelope.is_some()), "{name}");
            assert_eq!(method["mutates"], json!(resolved.mutates()), "{name}");
            if let Some(envelope) = envelope {
                assert!(
                    schema["mutation"][envelope.as_str().expect("an envelope name")].is_array(),
                    "{name}: its envelope {envelope} is described"
                );
            }
            assert_eq!(
                resolved.retries() != Retries::None,
                resolved.mutates(),
                "{name}: every mutating method, and only one, declares who answers its retry"
            );
            // Only a mutating handler can report a change, and only a method with an envelope has
            // one.
            if let Method::Host(host) = &resolved {
                match host.handler {
                    Handler::Service(_) | Handler::Planned(_) => {
                        assert!(!resolved.mutates(), "{name}: a read reports no change");
                    }
                    Handler::Mutating(_) => {
                        assert!(resolved.mutates(), "{name}: a change carries an envelope");
                    }
                    Handler::Owner(_) => {}
                }
            }
            if matches!(resolved.route(), Route::Service) {
                let response = call(&mut service, &mut session, name, json!({}));
                if let Some(error) = &response.error {
                    assert_ne!(error.code, "internal", "{name}: {}", error.message);
                }
            }
            assert!(
                !method["notes"].as_str().unwrap().is_empty(),
                "{name} has notes"
            );
        }
        assert!(
            !schema["coordinate_space"]
                .as_str()
                .expect("the coordinate note")
                .is_empty()
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// `resources.read` is a host method in the table: listed after `session.state` with notes
    /// that state its units and how a rate is derived, answered by its own handler with the
    /// documented objects, refusing parameters it does not take, and never the cause of an event.
    #[test]
    fn resources_read_is_listed_and_answers_through_the_method_table() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-resources-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut service = EditorService::open(&catalog).unwrap();
        let mut session = ClientSession::default();
        let names: Vec<&str> = METHODS.iter().map(|spec| spec.name).collect();
        let index = names
            .iter()
            .position(|name| *name == "resources.read")
            .expect("resources.read is a host method");
        assert_eq!(names[index - 1], "session.state");
        let schema = ok(&mut service, &mut session, "schema.list", json!({}));
        let listed = &schema["methods"]["resources.read"];
        assert_eq!(listed["mutates"], json!(false));
        assert_eq!(listed["required"], json!([]));
        assert_eq!(listed["optional"], json!({}));
        let notes = listed["notes"].as_str().expect("notes");
        for phrase in [
            "times are nanoseconds and sizes bytes",
            "100 × Δcpu.time_ns / Δmonotonic_ns",
            "unavailable maps its key to the reason",
            "needs no asset, emits no event and changes nothing",
        ] {
            assert!(notes.contains(phrase), "the notes say {phrase:?}");
        }
        let method = find(&service, "resources.read").expect("found");
        assert!(
            matches!(method.route(), Route::Service),
            "its service handler answers it"
        );
        for params in [json!({}), Value::Null] {
            let read = ok(&mut service, &mut session, "resources.read", params);
            let mut keys: Vec<&str> = read
                .as_object()
                .expect("an object")
                .keys()
                .map(String::as_str)
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, ["budgets", "cpu", "gpu", "memory", "monotonic_ns"]);
            assert!(read["cpu"]["logical_cpus"].is_u64());
            assert!(read["memory"]["kind"].is_string());
            assert!(read["budgets"]["colour_scratch"]["target_bytes"].is_u64());
            assert!(read["budgets"]["spatial"]["target_bytes"].is_u64());
            assert_eq!(
                method
                    .plan(&mut service, &mut session, &Value::Null)
                    .map(|(_, changed)| changed)
                    .expect("read"),
                Changed::Nothing,
                "the owner records no event for a read"
            );
        }
        for (params, message) in [
            (json!({"interval": 1}), "unknown field `interval`"),
            (json!([]), "params must be a JSON object"),
        ] {
            let error = call(&mut service, &mut session, "resources.read", params)
                .error
                .expect("refused");
            assert_eq!(error.code, "validation");
            assert!(error.message.contains(message), "{}", error.message);
        }
        assert_eq!(
            session,
            ClientSession::default(),
            "the session is untouched"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A declared task generates exactly one `task.<id>` method, listed by `schema.list` and
    /// resolved by the same lookup dispatch uses, and it is the catalog owner's to answer.
    #[test]
    fn a_declared_task_generates_one_owner_answered_method_that_discovery_and_dispatch_share() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-tasks-{}.sqlite",
            std::process::id()
        ));
        let mut registry = ModuleRegistry::builtin();
        registry
            .register(Arc::new(crate::CapabilitiesProofModule::new(
                "http://127.0.0.1:9",
            )))
            .unwrap();
        let mut service = EditorService::open_with(&catalog, Arc::new(registry)).unwrap();
        let mut session = ClientSession::default();
        let registry = service.registry().clone();
        let descriptors = registry.descriptors();
        let count = |kind: fn(&ModuleDescriptor) -> usize| -> usize {
            descriptors.iter().map(|descriptor| kind(descriptor)).sum()
        };
        let tasks: Vec<String> = descriptors
            .iter()
            .flat_map(|descriptor| descriptor.tasks.iter())
            .map(|task| task_method(&task.id))
            .collect();
        assert_eq!(tasks, ["task.generate-proof-tint"]);
        let schema = schemas(&registry);
        let listed = schema["methods"].as_object().unwrap();
        assert_eq!(
            listed.len(),
            METHODS.len()
                + crate::mask::commands::descriptor().actions.len()
                + crate::mask::commands::descriptor().queries.len()
                + count(|descriptor| descriptor.actions.len())
                + count(|descriptor| descriptor.queries.len())
                + tasks.len()
        );
        for name in listed.keys() {
            let method = find(&service, name).expect("every listed method resolves");
            assert_eq!(
                name.starts_with(TASK_PREFIX),
                matches!(method, Method::Task(_)),
                "{name}"
            );
        }
        let method = find(&service, &tasks[0]).unwrap();
        // The method carries the task's identity, which the owner hands the capability host.
        assert!(matches!(method.route(), Route::Task("generate-proof-tint")));
        assert!(method.mutates(), "the request queues a job");
        assert!(method.envelope() == Envelope::Request);
        assert_eq!(listed[&tasks[0]]["mutates"], json!(true));
        assert_eq!(listed[&tasks[0]]["mutation"], json!("request"));
        assert_eq!(listed[&tasks[0]]["required"][0], json!("mutation"));
        // The service never answers it: the catalog owner holds the capability host.
        let refused = method
            .serve(&mut service, &mut session, &json!({}))
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Internal);
        assert_eq!(
            listed[&tasks[0]]["parameters"],
            json!([]),
            "its declared parameters"
        );
        assert!(find(&service, "task.missing").is_none());
        assert!(find(&service, "generate-proof-tint").is_none());
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn render_locate_answers_a_view_point_in_the_content_stage() {
        let catalog =
            std::env::temp_dir().join(format!("luxforge-locate-{}.sqlite", std::process::id()));
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service
            .import(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg"),
            )
            .unwrap()
            .asset
            .id;
        let mut session = ClientSession::default();
        let original = service.state(&asset).unwrap().current_entry.id;

        // Discovery lists the method with its parameters before anyone calls it.
        let schema = call(&mut service, &mut session, "schema.list", json!({}))
            .result
            .expect("the schema");
        let listed = &schema["methods"]["render.locate"];
        assert_eq!(listed["mutates"], json!(false));
        assert_eq!(listed["required"], json!(["asset_id", "x", "y"]));
        assert!(
            listed["optional"]["entry_id"]
                .as_str()
                .expect("the optional entry")
                .contains("default"),
            "{listed}"
        );
        assert!(!listed["notes"].as_str().unwrap().is_empty());

        // A quarter turn puts the content stage's top-right corner in the output's top-left.
        let turned = call(
            &mut service,
            &mut session,
            "edit.transform",
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": 0, "request_id": "turn", "actor": "test"},
                "transform": "rotate-right",
            }),
        );
        assert!(turned.error.is_none(), "{:?}", turned.error);
        let revision = session.revision;
        let located = call(
            &mut service,
            &mut session,
            "render.locate",
            json!({"asset_id": asset, "x": 0, "y": 0}),
        )
        .result
        .expect("a located point");
        assert_eq!(
            located,
            json!({"content_x": 0, "content_y": 319, "width": 480, "height": 320})
        );
        assert_eq!(
            session.revision, revision,
            "locating changes no session state"
        );

        // A historical entry is located in its own stack: this point is outside the current one.
        let historical = call(
            &mut service,
            &mut session,
            "render.locate",
            json!({"asset_id": asset, "entry_id": original, "x": 479, "y": 0}),
        )
        .result
        .expect("a located point in the original entry");
        assert_eq!(
            historical,
            json!({"content_x": 479, "content_y": 0, "width": 480, "height": 320})
        );

        // A point outside the selected entry's output stage is refused, and names that stage.
        let error = call(
            &mut service,
            &mut session,
            "render.locate",
            json!({"asset_id": asset, "x": 320, "y": 0}),
        )
        .error
        .expect("a point outside the rendered image");
        assert_eq!(error.code, "validation");
        assert!(error.message.contains("320x480"), "{}", error.message);

        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// The same geometry `render.locate` walks a point through, as one matrix, because a gesture
    /// cannot ask per pointer move.
    #[test]
    fn render_transform_answers_the_geometry_tail_as_one_affine() {
        let catalog =
            std::env::temp_dir().join(format!("luxforge-transform-{}.sqlite", std::process::id()));
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service
            .import(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg"),
            )
            .unwrap()
            .asset
            .id;
        let mut session = ClientSession::default();
        let original = service.state(&asset).unwrap().current_entry.id;

        // Discovery lists the method beside its neighbours before anyone calls it.
        let schema = call(&mut service, &mut session, "schema.list", json!({}))
            .result
            .expect("the schema");
        let listed = &schema["methods"]["render.transform"];
        assert_eq!(listed["mutates"], json!(false));
        assert_eq!(listed["required"], json!(["asset_id"]));
        assert!(
            listed["optional"]["entry_id"]
                .as_str()
                .expect("the optional entry")
                .contains("default"),
            "{listed}"
        );
        assert!(!listed["notes"].as_str().unwrap().is_empty());

        let turned = call(
            &mut service,
            &mut session,
            "edit.transform",
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision": 0, "request_id": "turn", "actor": "test"},
                "transform": "rotate-right",
            }),
        );
        assert!(turned.error.is_none(), "{:?}", turned.error);
        let revision = session.revision;

        // A quarter turn right lays the content stage's y axis along the output's x axis, reversed:
        // content (0.5, 0.5), the first pixel's center, lands at output (319.5, 0.5).
        let mapped = call(
            &mut service,
            &mut session,
            "render.transform",
            json!({"asset_id": asset}),
        )
        .result
        .expect("the geometry tail as a matrix");
        assert_eq!(
            mapped,
            json!({
                "content": {"width": 480, "height": 320},
                "output": {"width": 320, "height": 480},
                "forward": [0.0, -1.0, 320.0, 1.0, 0.0, 0.0],
                "inverse": [0.0, 1.0, 0.0, -1.0, 0.0, 320.0],
            })
        );
        assert_eq!(
            session.revision, revision,
            "reading the transform changes no session state"
        );

        // A named entry answers for its own stack, which here is the untransformed original.
        let untouched = call(
            &mut service,
            &mut session,
            "render.transform",
            json!({"asset_id": asset, "entry_id": original}),
        )
        .result
        .expect("the original entry's matrix");
        assert_eq!(
            untouched,
            json!({
                "content": {"width": 480, "height": 320},
                "output": {"width": 480, "height": 320},
                "forward": [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                "inverse": [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
            })
        );

        // An entry that is not this asset's fails structurally, exactly as the neighbours do.
        let error = call(
            &mut service,
            &mut session,
            "render.transform",
            json!({"asset_id": asset, "entry_id": "entry-absent"}),
        )
        .error
        .expect("an unknown entry");
        assert_eq!(error.code, "validation");

        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// `preview.select` with `keep_geometry` frames the Original with the crop of the entry the
    /// session displays, and every question about the selection answers for that framing; without
    /// it the whole Original is selected, and selecting current carries no framing.
    #[test]
    fn preview_select_can_keep_the_displayed_geometry() {
        let catalog =
            std::env::temp_dir().join(format!("luxforge-framed-{}.sqlite", std::process::id()));
        let mut service = EditorService::open(&catalog).unwrap();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let mut session = ClientSession::default();
        let original = service.state(&asset).unwrap().current_entry.id;

        let schema = ok(&mut service, &mut session, "schema.list", json!({}));
        let listed = &schema["methods"]["preview.select"];
        assert_eq!(listed["mutates"], json!(false));
        assert!(
            listed["optional"]["keep_geometry"]
                .as_str()
                .expect("the optional flag")
                .contains("geometry"),
            "{listed}"
        );

        ok(
            &mut service,
            &mut session,
            "edit.crop",
            json!({"asset_id": asset, "mutation": mutation_json(0, "crop"), "x": 0.0, "y": 0.0, "width": 0.5, "height": 0.5}),
        );
        let cropped = service.state(&asset).unwrap().current_entry.id;
        let output = |service: &mut EditorService, session: &mut ClientSession, params: Value| {
            ok(service, session, "render.transform", params)["output"].clone()
        };
        let revision = session.revision;
        let framed = ok(
            &mut service,
            &mut session,
            "preview.select",
            json!({"asset_id": asset, "entry_id": original, "keep_geometry": true}),
        );
        assert_eq!(
            framed["session"]["preview"]["selections"],
            json!({asset.as_str(): {"entry_id": original, "geometry_from": cropped}}),
            "the framing is the entry that was displayed"
        );
        assert_eq!(
            output(&mut service, &mut session, json!({"asset_id": asset})),
            json!({"width": 240, "height": 160}),
            "the selection answers at the cropped size"
        );
        assert_eq!(
            output(
                &mut service,
                &mut session,
                json!({"asset_id": asset, "entry_id": original})
            ),
            json!({"width": 480, "height": 320}),
            "a named entry answers for its own stack"
        );
        assert!(
            call(
                &mut service,
                &mut session,
                "render.sample",
                json!({"asset_id": asset, "x": 300, "y": 0}),
            )
            .error
            .is_some(),
            "a point outside the framed Original is outside the image"
        );
        assert_eq!(
            service.revision(&asset).unwrap(),
            1,
            "nothing was committed"
        );
        assert!(session.revision > revision);

        // Selecting again while framed keeps the framing the session shows.
        let again = ok(
            &mut service,
            &mut session,
            "preview.select",
            json!({"asset_id": asset, "entry_id": original, "keep_geometry": true}),
        );
        assert_eq!(
            again["session"]["preview"]["selections"][asset.as_str()]["geometry_from"],
            json!(cropped)
        );

        // Without the flag the whole Original is shown.
        let whole = ok(
            &mut service,
            &mut session,
            "preview.select",
            json!({"asset_id": asset, "entry_id": original}),
        );
        assert_eq!(
            whole["session"]["preview"]["selections"][asset.as_str()],
            json!({"entry_id": original, "geometry_from": null})
        );
        assert_eq!(
            output(&mut service, &mut session, json!({"asset_id": asset})),
            json!({"width": 480, "height": 320})
        );

        // The current entry is the live state, never framed.
        let current = ok(
            &mut service,
            &mut session,
            "preview.select",
            json!({"asset_id": asset, "entry_id": cropped, "keep_geometry": true}),
        );
        assert_eq!(current["session"]["preview"]["selections"], json!({}));

        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A module with one number parameter that records what dispatch handed it.
    struct NumberModule {
        descriptor: ModuleDescriptor,
        seen: Arc<Mutex<Option<Map<String, Value>>>>,
    }

    impl ToolModule for NumberModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.descriptor
        }
        fn parse(
            &self,
            action_id: &str,
            parameters: &Map<String, Value>,
        ) -> Result<ActionInput, Error> {
            *self.seen.lock().expect("the recorded parameters") = Some(parameters.clone());
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: parameters.clone(),
            })
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::NoOp)
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe(&self, _: &str, _: u32, _: &Value) -> Result<crate::LayerReport, Error> {
            Ok(crate::LayerReport::new("Angle"))
        }
        fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
            Err(Error::internal("test module never renders"))
        }
    }

    #[test]
    fn a_generated_edit_method_passes_a_number_parameter_through_unchanged() {
        let seen = Arc::new(Mutex::new(None));
        let module = NumberModule {
            descriptor: ModuleDescriptor {
                id: "test.angle".into(),
                title: "Angle".into(),
                hint: None,
                effects: vec![EffectDescriptor {
                    id: "test.angle.effect".into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Geometry,
                    order: 0,
                    maskable: false,
                    artifacts: false,
                    single: false,
                    sources: Vec::new(),
                }],
                actions: vec![crate::ActionDescriptor {
                    id: "test-angle".into(),
                    title: "Set angle".into(),
                    notes: "test".into(),
                    patch: false,
                    parameters: vec![
                        ParameterDescriptor::number("angle", -45.0, 45.0)
                            .required(true)
                            .unit("deg")
                            .notes("test"),
                    ],
                }],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability: Availability::Available,
                ..ModuleDescriptor::default()
            },
            seen: seen.clone(),
        };
        let mut registry = ModuleRegistry::builtin();
        registry.register(Arc::new(module)).unwrap();
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-number-{}.sqlite",
            std::process::id()
        ));
        let mut service = EditorService::open_with(&catalog, Arc::new(registry)).unwrap();
        let asset = service
            .import(
                &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg"),
            )
            .unwrap()
            .asset
            .id;
        let mut session = ClientSession::default();
        let listed = schemas(service.registry());
        assert_eq!(
            listed["methods"]["edit.test-angle"]["parameters"][0]["kind"],
            json!("number")
        );
        let response = call(
            &mut service,
            &mut session,
            "edit.test-angle",
            json!({
                "asset_id": asset,
                "mutation": {"expected_revision":0,"request_id":"angle","actor":"test"},
                "angle": -3.5,
            }),
        );
        assert!(response.error.is_none(), "{:?}", response.error);
        assert_eq!(response.result.unwrap()["outcome"], json!("no-op"));
        assert_eq!(
            seen.lock()
                .expect("the recorded parameters")
                .as_ref()
                .expect("a parsed request")["angle"],
            json!(-3.5),
            "the number reached the module exactly as sent"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    const MARK_EFFECT: &str = "test.mark.effect";
    const MARK_ACTION: &str = "test-mark";

    /// A module that commits one identity layer, so a stack can hold an effect whose provider is
    /// later registered as unavailable or not registered at all.
    struct MarkModule(ModuleDescriptor);

    impl MarkModule {
        fn shared(availability: Availability) -> Arc<dyn ToolModule> {
            Arc::new(Self(ModuleDescriptor {
                id: "test.mark".into(),
                title: "Mark".into(),
                hint: None,
                effects: vec![EffectDescriptor {
                    id: MARK_EFFECT.into(),
                    format: EFFECT_FORMAT,
                    stage: EffectStage::Geometry,
                    order: 0,
                    maskable: false,
                    artifacts: false,
                    single: false,
                    sources: Vec::new(),
                }],
                actions: vec![crate::ActionDescriptor {
                    id: MARK_ACTION.into(),
                    title: "Mark".into(),
                    notes: "test".into(),
                    patch: false,
                    parameters: Vec::new(),
                }],
                queries: Vec::new(),
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
                collapsed: false,
                layout: crate::ModuleLayout::Stacked,
                availability,
                ..ModuleDescriptor::default()
            }))
        }

        fn registry(availability: Availability) -> Arc<ModuleRegistry> {
            let mut registry = ModuleRegistry::builtin();
            registry.register(Self::shared(availability)).unwrap();
            Arc::new(registry)
        }
    }

    impl ToolModule for MarkModule {
        fn descriptor(&self) -> &ModuleDescriptor {
            &self.0
        }
        fn parse(&self, action_id: &str, _: &Map<String, Value>) -> Result<ActionInput, Error> {
            Ok(ActionInput {
                action_id: action_id.into(),
                parameters: Map::new(),
            })
        }
        fn plan(&self, _: &ActionInput, _: &StageContext<'_>) -> Result<ActionPlan, Error> {
            Ok(ActionPlan::Commit(crate::NewLayer::new(
                MARK_EFFECT,
                json!({}),
            )))
        }
        fn validate_payload(&self, _: &str, _: u32, _: &Value) -> Result<(), Error> {
            Ok(())
        }
        fn describe(&self, _: &str, _: u32, _: &Value) -> Result<crate::LayerReport, Error> {
            Ok(crate::LayerReport::new("Marked"))
        }
        fn compile(&self, _: &str, _: u32, _: &Value, stage: Stage) -> Result<Processing, Error> {
            Ok(Processing::ExactGeometry(ExactGeometry {
                a: 1,
                b: 0,
                c: 0,
                d: 1,
                tx: 0,
                ty: 0,
                output_width: stage.width,
                output_height: stage.height,
            }))
        }
    }

    /// A row says whether its stored layer changes nothing, by the layer's own module: a Basic layer
    /// returned to 0 EV stays in the stack and reads neutral, and the orientation four quarter turns
    /// leave behind does too, while any other value is an edit.
    #[test]
    fn recipe_describe_reports_whether_each_layer_is_neutral() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-neutral-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut service = EditorService::open(&catalog).unwrap();
        let mut session = ClientSession::default();
        let asset = json!(service.import(&fixture()).unwrap().asset.id);
        let mut revision = 0;
        let mut edit = |service: &mut EditorService, action: &str, fields: Value| {
            let mut params = json!({
                "asset_id": asset,
                "mutation": {"expected_revision": revision, "request_id": format!("r{revision}"), "actor": "test"},
            });
            for (name, value) in fields.as_object().unwrap() {
                params[name] = value.clone();
            }
            ok(
                service,
                &mut ClientSession::default(),
                &format!("edit.{action}"),
                params,
            );
            revision += 1;
        };
        let rows = |service: &mut EditorService, session: &mut ClientSession| {
            ok(
                service,
                session,
                "recipe.describe",
                json!({"asset_id": asset}),
            )["layers"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| (row["effect"].clone(), row["neutral"].clone()))
                .collect::<Vec<_>>()
        };
        edit(&mut service, "set-basic", json!({"exposure": 0.5}));
        edit(
            &mut service,
            "transform",
            json!({"transform": "rotate-right"}),
        );
        assert_eq!(
            rows(&mut service, &mut session),
            [
                (json!(crate::BASIC_EFFECT), json!(false)),
                (json!(crate::ORIENTATION_EFFECT), json!(false)),
            ]
        );
        edit(&mut service, "set-basic", json!({"exposure": 0}));
        for _ in 0..3 {
            edit(
                &mut service,
                "transform",
                json!({"transform": "rotate-right"}),
            );
        }
        assert_eq!(
            rows(&mut service, &mut session),
            [
                (json!(crate::BASIC_EFFECT), json!(true)),
                (json!(crate::ORIENTATION_EFFECT), json!(true)),
            ],
            "both layers are still stored and neither changes anything"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// `module.list` reports each effect's declared source kinds and answers with the same modules
    /// whether a caller names the photo or filters the whole list itself with the one rule; and
    /// every action of a module that does not apply to a photo's kind is refused by that rule,
    /// worded from the descriptor. Nothing here names a module: the kind-specific ones are found by
    /// their declarations.
    #[test]
    fn module_list_and_actions_follow_each_modules_declared_sources() {
        use crate::SourceTag;
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-sources-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut service = EditorService::open(&catalog).unwrap();
        let mut session = ClientSession::default();
        let state = service.import(&fixture()).unwrap();
        assert_eq!(state.asset.source.tag(), SourceTag::Jpeg);
        let asset = json!(state.asset.id);

        // The tag an effect declares is the one `asset.state` reports for the photo's source.
        let reported = ok(
            &mut service,
            &mut session,
            "asset.state",
            json!({"asset_id": asset}),
        );
        assert_eq!(reported["asset"]["source"]["kind"], json!(SourceTag::Jpeg));

        let parse = |listed: &Value| -> Vec<ModuleDescriptor> {
            serde_json::from_value(listed["modules"].clone()).expect("module descriptors")
        };
        let every = ok(&mut service, &mut session, "module.list", json!({}));
        let modules = parse(&every);
        assert_eq!(
            modules.len(),
            service.registry().descriptors().len(),
            "without a photo, every module"
        );
        // An effect that exists on every kind describes itself as before; one that does not lists
        // its kinds, and the RAW development lists only RAW.
        let mut restricted = 0;
        for (module, listed) in modules.iter().zip(every["modules"].as_array().unwrap()) {
            for (effect, value) in module
                .effects
                .iter()
                .zip(listed["effects"].as_array().unwrap())
            {
                match effect.sources.as_slice() {
                    [] => assert!(value.get("sources").is_none(), "{}", effect.id),
                    kinds => {
                        restricted += 1;
                        assert_eq!(value["sources"], json!(kinds), "{}", effect.id);
                    }
                }
            }
        }
        let raw_only: Vec<&ModuleDescriptor> = modules
            .iter()
            .filter(|module| {
                module.applies_to(SourceTag::Raw) && !module.applies_to(SourceTag::Jpeg)
            })
            .collect();
        assert!(!raw_only.is_empty() && restricted > 0);
        assert!(raw_only.iter().all(|module| {
            module
                .effects
                .iter()
                .all(|effect| effect.sources == [SourceTag::Raw])
        }));

        // The same filtered result whether or not the photo is named.
        let named = parse(&ok(
            &mut service,
            &mut session,
            "module.list",
            json!({"asset_id": asset}),
        ));
        let filtered: Vec<ModuleDescriptor> = modules
            .iter()
            .filter(|module| module.applies_to(SourceTag::Jpeg))
            .cloned()
            .collect();
        assert_eq!(named, filtered);
        assert!(named.len() < modules.len());
        for kind in SourceTag::ALL {
            let resolved: Vec<&ModuleDescriptor> = modules
                .iter()
                .filter(|module| module.applies_to(kind))
                .collect();
            assert_eq!(
                listed_modules(service.registry(), Some(kind)),
                resolved,
                "{}",
                kind.label()
            );
        }

        // Every action of a module that does not apply to a JPEG is refused on one, before it
        // plans, with the descriptor's own words; nothing is written.
        let mut refused = 0;
        for module in &raw_only {
            for action in &module.actions {
                let mut params = json!({
                    "asset_id": asset,
                    "mutation": mutation_json(state.revision, &format!("refused-{}", action.id)),
                });
                for parameter in action.parameters.iter().filter(|p| p.required) {
                    params[&parameter.name] = match (&parameter.default, &parameter.kind) {
                        (Some(value), _) => value.clone(),
                        (None, crate::ParameterKind::Number { min, max }) => {
                            json!((min + max) / 2.0)
                        }
                        (None, crate::ParameterKind::Integer { min, .. }) => json!(min),
                        (None, crate::ParameterKind::Enum { options }) => json!(options[0]),
                        (None, crate::ParameterKind::Boolean) => json!(false),
                        (None, other) => panic!("no sample value for {other:?}"),
                    };
                }
                let response = call(
                    &mut service,
                    &mut session,
                    &format!("edit.{}", action.id),
                    params,
                );
                let error = response.error.expect("refused on a JPEG");
                assert_eq!(error.code, ErrorKind::Validation.code(), "{}", action.id);
                assert_eq!(
                    error.message,
                    format!("{} does not apply to a JPEG photo", module.title),
                    "{}",
                    action.id
                );
                refused += 1;
            }
        }
        assert!(refused > 0);
        assert_eq!(
            service.state(&state.asset.id).unwrap().revision,
            state.revision,
            "a refused action writes nothing"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn recipe_describe_lists_every_layer_and_names_a_provider_it_cannot_use() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-describe-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let asset;
        {
            let mut service =
                EditorService::open_with(&catalog, MarkModule::registry(Availability::Available))
                    .unwrap();
            let mut session = ClientSession::default();
            asset = json!(service.import(&fixture()).unwrap().asset.id);
            ok(
                &mut service,
                &mut session,
                "edit.transform",
                json!({"asset_id":asset,"mutation":{"expected_revision":0,"request_id":"t","actor":"test"},"transform":"rotate-left"}),
            );
            ok(
                &mut service,
                &mut session,
                &format!("edit.{MARK_ACTION}"),
                json!({"asset_id":asset,"mutation":{"expected_revision":1,"request_id":"m","actor":"test"}}),
            );
            let described = ok(
                &mut service,
                &mut session,
                "recipe.describe",
                json!({"asset_id": asset}),
            );
            assert_eq!(
                described["layers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|layer| (
                        layer["module"].clone(),
                        layer["summary"].clone(),
                        layer["available"].clone()
                    ))
                    .collect::<Vec<_>>(),
                [
                    (
                        json!("luxforge.transform"),
                        json!("Rotate left"),
                        json!(true)
                    ),
                    (json!("test.mark"), json!("Marked"), json!(true)),
                ]
            );
            assert_eq!(described["layers"][1]["effect"], json!(MARK_EFFECT));
            assert_eq!(described["layers"][1]["title"], json!("Mark"));
            // Each row names the stage its layer receives: the fixture's own, then the stage the
            // quarter turn produced, which is what the mark's payload addresses.
            assert_eq!(
                (
                    &described["layers"][0]["input_stage"],
                    &described["layers"][1]["input_stage"]
                ),
                (
                    &json!({"width": 480, "height": 320}),
                    &json!({"width": 320, "height": 480})
                )
            );
            assert_eq!(
                described["entry_id"],
                ok(
                    &mut service,
                    &mut session,
                    "asset.state",
                    json!({"asset_id": asset})
                )["current_entry"]["id"]
            );
        }
        // The same catalog served by a provider that reports itself unavailable.
        {
            let mut service = EditorService::open_with(
                &catalog,
                MarkModule::registry(Availability::Unavailable {
                    reason: "not built in this configuration".into(),
                }),
            )
            .unwrap();
            let mut session = ClientSession::default();
            let described = ok(
                &mut service,
                &mut session,
                "recipe.describe",
                json!({"asset_id": asset}),
            );
            let layer = &described["layers"][1];
            assert_eq!(layer["available"], json!(false));
            assert_eq!(
                layer["summary"],
                json!("unavailable: not built in this configuration")
            );
            assert_eq!(
                layer["module"],
                json!("test.mark"),
                "an unavailable provider keeps its identity"
            );
            assert_eq!(described["layers"][0]["available"], json!(true));
        }
        // And with no provider registered for that effect at all.
        {
            let mut service = EditorService::open(&catalog).unwrap();
            let mut session = ClientSession::default();
            let described = ok(
                &mut service,
                &mut session,
                "recipe.describe",
                json!({"asset_id": asset}),
            );
            let layer = &described["layers"][1];
            assert_eq!(layer["available"], json!(false));
            assert_eq!(layer["summary"], json!("no provider"));
            assert_eq!(layer["module"], json!(null));
            assert_eq!(layer["title"], json!(null));
            assert_eq!(layer["effect"], json!(MARK_EFFECT));
        }
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn workspace_state_round_trips_through_session_state_and_validates_the_mode() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-workspace-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut service =
            EditorService::open_with(&catalog, Arc::new(ModuleRegistry::developer())).unwrap();
        let mut session = ClientSession::default();
        assert_eq!(
            ok(&mut service, &mut session, "session.state", json!({}))["workspace"],
            json!({
                "state_panel": true,
                "tools_panel": true,
                "mode": "pointer",
                "thirds": false,
                "clip_shadows": false,
                "clip_highlights": false,
                "mask_overlay": "off",
                "mask_overlay_colour": "green",
            }),
            "a fresh session opens with both panels, the pointer and no overlay"
        );
        let set = ok(
            &mut service,
            &mut session,
            "workspace.set",
            json!({"state_panel": false, "mode": "luxforge.crop", "thirds": true}),
        );
        assert_eq!(
            set["workspace"],
            json!({
                "state_panel": false,
                "tools_panel": true,
                "mode": "luxforge.crop",
                "thirds": true,
                "clip_shadows": false,
                "clip_highlights": false,
                "mask_overlay": "off",
                "mask_overlay_colour": "green",
            })
        );
        assert_eq!(set["revision"], json!(1), "a session change is a revision");
        assert_eq!(
            ok(&mut service, &mut session, "session.state", json!({}))["workspace"],
            set["workspace"],
            "session.state reports what workspace.set stored"
        );
        for (case, params, fragment) in [
            (
                "an unknown mode",
                json!({"mode": "luxforge.heal"}),
                // The host modes first — the pointer, the mask mode and one per canvas pick the host
                // declares for a sampling component kind — then the registry's canvas declarations,
                // so the RAW and Basic neutral pickers join the list without a change here.
                "mode must be one of pointer, mask, mask.add-colour-range-sample, luxforge.pixel, \
                 luxforge.raw, luxforge.basic, luxforge.crop",
            ),
            (
                "a module that declares no canvas",
                json!({"mode": "luxforge.transform"}),
                "mode must be one of",
            ),
            ("an unknown field", json!({"panel": true}), "unknown field"),
            (
                "the wrong type",
                json!({"thirds": "yes"}),
                "parameter thirds must be a boolean",
            ),
        ] {
            let error = call(&mut service, &mut session, "workspace.set", params)
                .error
                .unwrap_or_else(|| panic!("{case} must be refused"));
            assert_eq!(error.code, "validation", "{case}");
            assert!(
                error.message.contains(fragment),
                "{case}: {}",
                error.message
            );
        }
        assert_eq!(
            ok(&mut service, &mut session, "session.state", json!({}))["workspace"],
            set["workspace"],
            "a refused request changes nothing"
        );
        assert_eq!(
            ok(&mut service, &mut session, "workspace.set", json!({}))["workspace"],
            set["workspace"],
            "an empty request keeps the state"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// `schema.list` says everything a client needs to post a path, so an agent draws the stroke a
    /// hand draws without reading any desktop code — and it says it under `paths`, with no mention
    /// of the one feature that happens to use it first.
    #[test]
    fn schema_list_publishes_the_path_primitives_without_naming_a_feature() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-paths-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut service = EditorService::open(&catalog).unwrap();
        let mut session = ClientSession::default();
        let listed = ok(&mut service, &mut session, "schema.list", json!({}));
        let paths = &listed["paths"];

        assert_eq!(
            paths["range"],
            json!([path::COORDINATE_MIN, path::COORDINATE_MAX])
        );
        assert_eq!(paths["stored_steps_per_unit"], json!(16384.0));
        assert_eq!(paths["decimation_tolerance_of_radius"], json!(0.04));
        assert_eq!(
            paths["decimation_tolerance_min"],
            json!(2.0 / 16384.0),
            "two grid steps"
        );
        assert_eq!(paths["grid_rounding"], json!(path::GRID_ROUNDING));
        assert_eq!(paths["points_per_stroke"], json!(1024));
        assert_eq!(
            paths["posted_points_per_stroke"],
            json!(16 * 1024),
            "a raw path is bounded before decimation, and more generously than the stroke after it"
        );
        for text in [&paths["coordinates"], &paths["notes"]] {
            let text = text.as_str().expect("prose a client can read");
            assert!(!text.to_lowercase().contains("mask"), "{text}");
            assert!(!text.to_lowercase().contains("brush"), "{text}");
        }
        assert!(
            paths["notes"]
                .as_str()
                .unwrap()
                .contains("the same posted path always produces the same stored stroke")
        );
        // The bound that is the mask's own is published with the masks, not with the paths.
        assert!(paths.get("points_per_mask").is_none());
        assert_eq!(
            listed["masks"]["points_per_mask"],
            json!(crate::POINTS_PER_MASK)
        );

        // And the kind itself serializes flat, as every other parameter kind does.
        let declared = ParameterDescriptor::points("path", 1, 512)
            .required(true)
            .notes("the drawn path");
        let listed_kind = serde_json::to_value(&declared).unwrap();
        assert_eq!(listed_kind["kind"], json!("points"));
        assert_eq!(listed_kind["points_min"], json!(1));
        assert_eq!(listed_kind["points_max"], json!(512));
        assert_eq!(
            serde_json::from_value::<crate::ParameterDescriptor>(listed_kind).unwrap(),
            declared
        );
        drop(service);
        let _ = std::fs::remove_file(&catalog);
    }

    #[test]
    fn the_components_gallery_page_is_not_session_state() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-gallery-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut service = EditorService::open(&catalog).unwrap();
        let mut session = ClientSession::default();

        // The developer components gallery is the desktop's own view state: `workspace.set`
        // declares no page, the session reports none, and a page sent anyway is refused as an
        // unknown field without changing the session.
        let listed = ok(&mut service, &mut session, "schema.list", json!({}));
        let workspace_set = &listed["methods"]["workspace.set"];
        assert_eq!(workspace_set["mutates"], json!(false));
        assert!(workspace_set["optional"].get("component_gallery").is_none());
        let state = ok(&mut service, &mut session, "session.state", json!({}));
        assert!(state["workspace"].get("component_gallery").is_none());
        let preserved = session.clone();
        let rejected = call(
            &mut service,
            &mut session,
            "workspace.set",
            json!({"component_gallery": 0, "tools_panel": false}),
        );
        assert_eq!(rejected.error.unwrap().code, "validation");
        assert_eq!(session, preserved, "a refused page changes nothing");
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A service serving the built-ins plus the test patch module, with one asset imported. The
    /// patch module is the shape a field-patch tool takes: one layer, merged field by field.
    fn patched(name: &str) -> (EditorService, PathBuf, Value) {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-{name}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut registry = ModuleRegistry::builtin();
        registry.register(PatchModule::shared()).unwrap();
        let mut service = EditorService::open_with(&catalog, Arc::new(registry)).unwrap();
        let asset = json!(service.import(&fixture()).unwrap().asset.id);
        (service, catalog, asset)
    }

    fn patch_method() -> String {
        format!("edit.{PATCH_ACTION}")
    }

    /// The entry a mutation result points at.
    fn entry_of(
        service: &mut EditorService,
        session: &mut ClientSession,
        asset: &Value,
        result: &Value,
    ) -> Value {
        ok(
            service,
            session,
            "history.inspect",
            json!({"asset_id": asset, "entry_id": result["current_entry_id"]}),
        )
    }

    fn described(service: &mut EditorService, session: &mut ClientSession, asset: &Value) -> Value {
        ok(
            service,
            session,
            "recipe.describe",
            json!({"asset_id": asset}),
        )
    }

    /// `edit.apply-preset` takes its settings, name and library identity as top-level fields beside
    /// the envelope, and any registered field patch is presettable: the test patch module's action
    /// is applied in the same entry as Basic's. A second identical call is a no-op that emits no
    /// event, and a refused step is the same structured error the action gives alone.
    #[test]
    fn a_preset_applies_through_its_generated_method_as_one_entry() {
        let (mut service, catalog, asset) = patched("preset");
        let mut session = ClientSession::default();
        let settings = json!({"set-patch": {"red": 12.0}, "set-basic": {"exposure": 0.5}});
        let (applied, changed) = mutated(
            &mut service,
            &mut session,
            "edit.apply-preset",
            json!({
                "asset_id": asset,
                "mutation": mutation_json(0, "preset"),
                "settings": settings,
                "name": "Warm",
                "preset-id": "preset-7",
            }),
        );
        assert_eq!(applied["outcome"], json!("applied"));
        assert_eq!(applied["revision"], json!(1));
        assert_eq!(changed, Changed::Something { revision: Some(1) });
        let entry = entry_of(&mut service, &mut session, &asset, &applied);
        assert_eq!(entry["action_id"], json!("apply-preset"));
        assert_eq!(entry["label"], json!("Preset: Warm"));
        assert_eq!(
            entry["parameters"],
            json!({"settings": settings, "name": "Warm", "preset-id": "preset-7"})
        );
        let rows = described(&mut service, &mut session, &asset);
        assert_eq!(
            rows["layers"]
                .as_array()
                .expect("the layer rows")
                .iter()
                .map(|row| (row["module"].clone(), row["values"]["red"].clone()))
                .collect::<Vec<_>>(),
            [
                (json!("luxforge.basic"), Value::Null),
                (json!(PATCH_MODULE), json!(12.0)),
            ],
            "steps run in key order, so the patch layer joins the content region after Basic's, \
             exactly as sending the two actions in that order does"
        );

        let (again, changed) = mutated(
            &mut service,
            &mut session,
            "edit.apply-preset",
            json!({
                "asset_id": asset,
                "mutation": mutation_json(1, "again"),
                "settings": settings,
                "name": "Warm",
            }),
        );
        assert_eq!(again["outcome"], json!("no-op"));
        assert_eq!(again["created_entry_id"], json!(null));
        assert_eq!(changed, Changed::Nothing);

        let refused = call(
            &mut service,
            &mut session,
            "edit.apply-preset",
            json!({
                "asset_id": asset,
                "mutation": mutation_json(1, "refused"),
                "settings": {"set-patch": {"red": 300}},
                "name": "Too red",
            }),
        );
        let error = refused.error.expect("a refused step");
        assert_eq!(error.code, "validation");
        assert_eq!(
            error.message,
            "parameter red must be a number within 0..=255"
        );
        assert_eq!(
            service
                .state(&serde_json::from_value(asset.clone()).unwrap())
                .unwrap()
                .revision,
            1,
            "nothing was written"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_patch_action_merges_the_fields_it_was_sent_and_reports_them_on_the_recipe_row() {
        let (mut service, catalog, asset) = patched("patch");
        let mut session = ClientSession::default();
        // Discovery says it is a patch: no parameter is required, and both carry decimal hints.
        let schema = schemas(service.registry());
        let listed = &schema["methods"][patch_method()];
        assert_eq!(listed["patch"], json!(true));
        assert_eq!(listed["required"], json!(["asset_id", "mutation"]));
        assert_eq!(
            listed["optional"]
                .as_object()
                .expect("the patch fields")
                .keys()
                .collect::<Vec<_>>(),
            ["green", "red"]
        );
        assert_eq!(listed["parameters"][0]["step"], json!(1.0));
        assert_eq!(listed["parameters"][0]["precision"], json!(0));
        assert_eq!(
            schema["methods"]["edit.crop"]["patch"],
            json!(false),
            "an ordinary action is not a patch and keeps its required fields"
        );
        assert_eq!(
            schema["methods"]["edit.crop"]["required"],
            json!(["asset_id", "mutation", "x", "y", "width", "height"])
        );

        let patch = |service: &mut EditorService,
                     session: &mut ClientSession,
                     revision: u64,
                     request: &str,
                     fields: Value| {
            let mut params = fields;
            params["asset_id"] = asset.clone();
            params["mutation"] = mutation_json(revision, request);
            ok(service, session, &patch_method(), params)
        };

        // One field commits the module's one layer and the module labels the entry.
        let first = patch(&mut service, &mut session, 0, "red", json!({"red": 12.0}));
        assert_eq!(first["outcome"], json!("applied"));
        let entry = entry_of(&mut service, &mut session, &asset, &first);
        assert_eq!(entry["label"], json!("Patch red 12"));
        assert_eq!(
            entry["parameters"],
            json!({"red": 12.0}),
            "the entry stores the patch as sent, not the merged payload"
        );
        let rows = described(&mut service, &mut session, &asset);
        let layer_id = rows["layers"][0]["id"].clone();
        assert_eq!(rows["layers"].as_array().unwrap().len(), 1);
        assert_eq!(rows["layers"][0]["module"], json!(PATCH_MODULE));
        assert_eq!(
            rows["layers"][0]["values"],
            json!({"red": 12.0, "green": 0.0})
        );

        // A second field merges into the same layer, which keeps its identity and position.
        let second = patch(
            &mut service,
            &mut session,
            1,
            "green",
            json!({"green": 30.0}),
        );
        assert_eq!(second["outcome"], json!("applied"));
        assert_eq!(
            entry_of(&mut service, &mut session, &asset, &second)["label"],
            json!("Patch green 30")
        );
        let rows = described(&mut service, &mut session, &asset);
        assert_eq!(rows["layers"].as_array().unwrap().len(), 1);
        assert_eq!(rows["layers"][0]["id"], layer_id);
        assert_eq!(
            rows["layers"][0]["values"],
            json!({"red": 12.0, "green": 30.0})
        );

        // The same value again changes nothing, so no entry is written.
        let again = patch(
            &mut service,
            &mut session,
            2,
            "again",
            json!({"green": 30.0}),
        );
        assert_eq!(again["outcome"], json!("no-op"));
        assert_eq!(again["revision"], json!(2));
        assert_eq!(again["created_entry_id"], json!(null));

        // A patch the module has nothing to say about is labelled with the action's title.
        let both = patch(
            &mut service,
            &mut session,
            2,
            "both",
            json!({"red": 1.0, "green": 2.0}),
        );
        assert_eq!(
            entry_of(&mut service, &mut session, &asset, &both)["label"],
            json!("Set patch")
        );

        // The crop module reports its frame the same way, so a client seeds its controls from the
        // displayed entry instead of parsing payloads itself.
        ok(
            &mut service,
            &mut session,
            "edit.crop",
            json!({"asset_id": asset, "mutation": mutation_json(3, "crop"), "x": 0.0, "y": 0.0, "width": 0.5, "height": 0.5}),
        );
        let rows = described(&mut service, &mut session, &asset);
        let crop = rows["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|layer| layer["module"] == json!("luxforge.crop"))
            .expect("the crop layer")
            .clone();
        assert_eq!(
            crop["values"],
            json!({"angle": 0.0, "x": 0.0, "y": 0.0, "width": 0.5, "height": 0.5})
        );
        assert_eq!(
            rows["layers"][0]["values"],
            json!({"red": 1.0, "green": 2.0}),
            "the patch layer stays before the geometry tail with its own values"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_draft_begins_sets_reads_and_commits_one_entry() {
        let (mut service, catalog, asset) = patched("draft");
        let mut session = ClientSession::default();
        let begun = ok(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": PATCH_ACTION}),
        );
        let draft_id = begun["draft_id"].clone();
        assert!(
            draft_id.as_str().expect("a draft id").starts_with("draft-"),
            "{draft_id}"
        );
        assert_eq!(begun["action"], json!(PATCH_ACTION));
        assert_eq!(begun["asset_id"], asset);
        assert_eq!(begun["base_revision"], json!(0));
        assert_eq!(begun["draft_revision"], json!(0));
        assert_eq!(begun["fields"], json!({}));
        assert_eq!(begun["conflicted"], json!(false));
        assert_eq!(
            ok(&mut service, &mut session, "session.state", json!({}))["draft"],
            begun,
            "the session reports the open draft"
        );

        // Every set validates and merges; the draft revision counts the steps of the gesture.
        let set = ok(
            &mut service,
            &mut session,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"red": 10.0}}),
        );
        assert_eq!(set["fields"], json!({"red": 10.0}));
        assert_eq!(set["draft_revision"], json!(1));
        let set = ok(
            &mut service,
            &mut session,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"red": 20.0}}),
        );
        assert_eq!(set["fields"], json!({"red": 20.0}));
        assert_eq!(set["draft_revision"], json!(2));
        assert_eq!(
            ok(
                &mut service,
                &mut session,
                "draft.read",
                json!({"draft_id": draft_id})
            ),
            set
        );

        // Nothing is committed while the draft is open.
        assert_eq!(
            ok(
                &mut service,
                &mut session,
                "asset.state",
                json!({"asset_id": asset})
            )["revision"],
            json!(0)
        );
        assert_eq!(
            described(&mut service, &mut session, &asset)["layers"],
            json!([])
        );

        // A sample of the draft shows what committing would produce; the stored stack does not.
        let drafted = ok(
            &mut service,
            &mut session,
            "render.sample",
            json!({"asset_id": asset, "x": 0, "y": 0, "draft_id": draft_id}),
        );
        assert_eq!(drafted["rgba"], json!([20, 0, 0, 255]));
        assert_eq!(
            drafted["draft"],
            json!({"draft_id": draft_id, "draft_revision": 2})
        );
        let stored = ok(
            &mut service,
            &mut session,
            "render.sample",
            json!({"asset_id": asset, "x": 0, "y": 0}),
        );
        assert_ne!(stored["rgba"], drafted["rgba"]);
        assert_eq!(stored["draft"], json!(null));

        let committed = ok(
            &mut service,
            &mut session,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation_json(0, "gesture")}),
        );
        assert_eq!(committed["outcome"], json!("applied"));
        assert_eq!(committed["revision"], json!(1));
        assert_eq!(
            entry_of(&mut service, &mut session, &asset, &committed)["label"],
            json!("Patch red 20")
        );
        assert_eq!(
            ok(&mut service, &mut session, "session.state", json!({}))["draft"],
            json!(null),
            "committing ends the draft"
        );
        assert_eq!(
            ok(
                &mut service,
                &mut session,
                "render.sample",
                json!({"asset_id": asset, "x": 0, "y": 0})
            )["rgba"],
            drafted["rgba"],
            "the committed stack now produces what the draft previewed"
        );
        let error = call(
            &mut service,
            &mut session,
            "draft.read",
            json!({"draft_id": draft_id}),
        )
        .error
        .expect("the draft has ended");
        assert_eq!(error.code, "validation");
        assert!(error.message.contains("unknown draft"), "{}", error.message);
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A draft is refused up front wherever its commit would be for what the photo is, in the
    /// commit's words: a RAW development at `draft.begin` on a JPEG, and Basic's Temperature at the
    /// `draft.set` that sets it on a RAW photo's global target, which leaves the draft as it was.
    /// The same field through a mask is Basic's relative white balance, and drafts.
    #[test]
    fn a_draft_is_refused_where_its_commit_would_be_for_the_photos_kind() {
        let catalog = std::env::temp_dir().join(format!(
            "luxforge-methods-draft-kind-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut service = EditorService::open(&catalog).unwrap();
        let mut session = ClientSession::default();
        let asset = service.import(&fixture()).unwrap().asset.id;
        let refusal = |response: ApiResponse| {
            let error = response.error.expect("refused");
            (error.code, error.message)
        };

        let committed = refusal(call(
            &mut service,
            &mut session,
            "edit.set-raw",
            json!({"asset_id": asset, "mutation": mutation_json(0, "raw"), "temperature": 5000.0}),
        ));
        assert_eq!(
            committed,
            (
                "validation".to_owned(),
                "RAW does not apply to a JPEG photo".to_owned()
            )
        );
        let begun = call(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-raw"}),
        );
        assert_eq!(refusal(begun), committed);
        assert!(session.draft.is_none(), "no draft opened");

        // A mask made while the photo is a JPEG, then the row recast as a RAW photo's.
        ok(
            &mut service,
            &mut session,
            "mask.create-linear",
            json!({"asset_id": asset, "mutation": mutation_json(0, "mask"),
                   "x0": 0.0, "y0": 0.0, "x1": 0.0, "y1": 1.0}),
        );
        let state = service.state(&asset).unwrap();
        let mask = state.current_entry.snapshot.recipe.masks[0].id.clone();
        drop(service);
        crate::editor::recast_as_raw(&catalog, &asset);
        let mut service = EditorService::open(&catalog).unwrap();
        let mut session = ClientSession::default();

        let committed = refusal(call(
            &mut service,
            &mut session,
            "edit.set-basic",
            json!({"asset_id": asset, "mutation": mutation_json(state.revision, "wb"),
                   "temperature": 20.0}),
        ));
        assert_eq!(
            committed,
            (
                "validation".to_owned(),
                "on a RAW photo, Temperature is the source development's: set-raw temperature (K)"
                    .to_owned()
            )
        );
        let begun = ok(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-basic"}),
        );
        let draft_id = begun["draft_id"].clone();
        ok(
            &mut service,
            &mut session,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"exposure": 0.5}}),
        );
        let set = call(
            &mut service,
            &mut session,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"exposure": 1.0, "temperature": 20.0}}),
        );
        assert_eq!(refusal(set), committed);
        let read = ok(
            &mut service,
            &mut session,
            "draft.read",
            json!({"draft_id": draft_id}),
        );
        assert_eq!(read["fields"], json!({"exposure": 0.5}), "nothing merged");
        assert_eq!(read["draft_revision"], json!(1));
        ok(
            &mut service,
            &mut session,
            "draft.cancel",
            json!({"draft_id": draft_id}),
        );

        let masked = ok(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-basic", "mask": mask}),
        );
        let set = ok(
            &mut service,
            &mut session,
            "draft.set",
            json!({"draft_id": masked["draft_id"], "fields": {"temperature": 20.0}}),
        );
        assert_eq!(set["fields"], json!({"temperature": 20.0}));
        ok(
            &mut service,
            &mut session,
            "draft.cancel",
            json!({"draft_id": masked["draft_id"]}),
        );
        ok(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-raw"}),
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A draft over an ordinary, non-patch action whose one parameter is the whole request: the
    /// gesture a desktop slider of such a control makes. `draft.set` validates the one field,
    /// `draft_recipe` plans the drafted action against the stored stack without persisting it, and
    /// `draft.commit` applies it as exactly one history entry. No module is named by the draft
    /// machinery; the transform module is used here because it is registered and declares exactly
    /// one parameter, which is the shape the rule turns on.
    #[test]
    fn a_draft_over_a_single_parameter_action_previews_and_commits_one_entry() {
        let (mut service, catalog, asset) = patched("draft-single");
        let mut session = ClientSession::default();
        let single = service
            .registry()
            .action("transform")
            .expect("the transform action is registered")
            .1
            .clone();
        assert!(!single.patch, "the action under test is not a field patch");
        assert_eq!(
            single.parameters.len(),
            1,
            "its one parameter is the whole request"
        );
        let asset_id: AssetId = serde_json::from_value(asset.clone()).expect("the asset id");

        let begun = ok(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": single.id}),
        );
        let draft_id = begun["draft_id"].clone();
        assert_eq!(begun["fields"], json!({}));
        assert_eq!(begun["base_revision"], json!(0));

        let set = ok(
            &mut service,
            &mut session,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {single.parameters[0].name.clone(): "rotate-right"}}),
        );
        assert_eq!(set["fields"], json!({"transform": "rotate-right"}));
        assert_eq!(set["draft_revision"], json!(1));

        // The preview: the recipe the open draft would produce, planned and never persisted.
        let held = session.draft.clone().expect("the open draft");
        let (drafted, _) = service
            .draft_recipe(&asset_id, &held)
            .expect("the drafted recipe");
        assert_eq!(
            drafted.layers.len(),
            1,
            "the drafted recipe carries the action's own layer: {drafted:?}"
        );
        assert_eq!(
            described(&mut service, &mut session, &asset)["layers"],
            json!([]),
            "and the stored stack still holds nothing"
        );

        let committed = ok(
            &mut service,
            &mut session,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation_json(0, "single")}),
        );
        assert_eq!(committed["outcome"], json!("applied"));
        assert_eq!(committed["revision"], json!(1));
        assert_eq!(
            described(&mut service, &mut session, &asset)["layers"]
                .as_array()
                .expect("the committed layers")
                .len(),
            1,
            "one gesture is one entry and one layer"
        );
        assert_eq!(
            ok(&mut service, &mut session, "session.state", json!({}))["draft"],
            json!(null),
            "committing ends the draft"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_draft_is_refused_while_one_is_open_or_a_historical_entry_is_previewed() {
        let (mut service, catalog, asset) = patched("draft-refused");
        let mut session = ClientSession::default();
        let original = ok(
            &mut service,
            &mut session,
            "asset.state",
            json!({"asset_id": asset}),
        )["current_entry"]["id"]
            .clone();
        let unknown = call(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": "set-nothing"}),
        )
        .error
        .expect("an unknown action cannot be drafted");
        assert_eq!(unknown.code, "validation");
        assert!(unknown.message.contains("unknown action set-nothing"));

        let begun = ok(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": PATCH_ACTION}),
        );
        let draft_id = begun["draft_id"].clone();
        let second = call(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": PATCH_ACTION}),
        )
        .error
        .expect("one draft per client");
        assert_eq!(second.code, "conflict");
        assert!(
            second.message.contains("already holds draft"),
            "{}",
            second.message
        );
        assert_eq!(
            ok(
                &mut service,
                &mut session,
                "draft.cancel",
                json!({"draft_id": draft_id})
            ),
            json!({"cancelled": true})
        );
        assert_eq!(
            ok(&mut service, &mut session, "session.state", json!({}))["draft"],
            json!(null),
            "cancelling ends the draft and commits nothing"
        );
        assert_eq!(
            ok(
                &mut service,
                &mut session,
                "asset.state",
                json!({"asset_id": asset})
            )["revision"],
            json!(0)
        );
        assert_eq!(
            call(
                &mut service,
                &mut session,
                "draft.cancel",
                json!({"draft_id": draft_id})
            )
            .error
            .expect("the draft is gone")
            .code,
            "validation"
        );

        // A historical preview is read-only, so no gesture may start there.
        ok(
            &mut service,
            &mut session,
            &patch_method(),
            json!({"asset_id": asset, "mutation": mutation_json(0, "one"), "red": 5.0}),
        );
        ok(
            &mut service,
            &mut session,
            "preview.select",
            json!({"asset_id": asset, "entry_id": original}),
        );
        let previewing = call(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": PATCH_ACTION}),
        )
        .error
        .expect("a historical preview cannot be edited");
        assert_eq!(previewing.code, "validation");
        assert!(
            previewing.message.contains("return to current"),
            "{}",
            previewing.message
        );
        ok(
            &mut service,
            &mut session,
            "preview.return-current",
            json!({}),
        );
        assert!(
            ok(
                &mut service,
                &mut session,
                "draft.begin",
                json!({"asset_id": asset, "action": PATCH_ACTION})
            )["draft_id"]
                .is_string(),
            "returning to current allows the gesture again"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn an_invalid_field_leaves_the_draft_exactly_as_it_was() {
        let (mut service, catalog, asset) = patched("draft-invalid");
        let mut session = ClientSession::default();
        let draft_id = ok(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": PATCH_ACTION}),
        )["draft_id"]
            .clone();
        let good = ok(
            &mut service,
            &mut session,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"red": 10.0}}),
        );
        for (case, fields, fragment) in [
            (
                "unknown field",
                json!({"blue": 1.0}),
                "unknown parameter blue",
            ),
            (
                "out of range",
                json!({"red": 300.0}),
                "parameter red must be a number within 0..=255",
            ),
            (
                "not finite",
                json!({"red": f64::NAN}),
                "parameter red must be a number",
            ),
            (
                "wrong kind",
                json!({"red": "10"}),
                "parameter red must be a number",
            ),
            (
                // One bad field rejects the whole request: a draft never half-applies a set.
                "a good field beside a bad one",
                json!({"green": 5.0, "blue": 1.0}),
                "unknown parameter blue",
            ),
        ] {
            let error = call(
                &mut service,
                &mut session,
                "draft.set",
                json!({"draft_id": draft_id, "fields": fields}),
            )
            .error
            .unwrap_or_else(|| panic!("{case} must be refused"));
            assert_eq!(error.code, "validation", "{case}");
            assert!(
                error.message.contains(fragment),
                "{case}: {}",
                error.message
            );
            assert_eq!(
                ok(
                    &mut service,
                    &mut session,
                    "draft.read",
                    json!({"draft_id": draft_id})
                ),
                good,
                "{case} changed the draft"
            );
        }
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_gesture_that_returns_to_its_start_commits_nothing_and_ends_the_draft() {
        let (mut service, catalog, asset) = patched("draft-noop");
        let mut session = ClientSession::default();
        ok(
            &mut service,
            &mut session,
            &patch_method(),
            json!({"asset_id": asset, "mutation": mutation_json(0, "start"), "red": 10.0}),
        );
        let entries = |service: &mut EditorService, session: &mut ClientSession| -> usize {
            ok(service, session, "history.list", json!({"asset_id": asset}))["entries"]
                .as_array()
                .expect("the history page")
                .len()
        };
        let before = entries(&mut service, &mut session);
        let draft_id = ok(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": PATCH_ACTION}),
        )["draft_id"]
            .clone();
        for value in [30.0, 10.0] {
            ok(
                &mut service,
                &mut session,
                "draft.set",
                json!({"draft_id": draft_id, "fields": {"red": value}}),
            );
        }
        let committed = ok(
            &mut service,
            &mut session,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation_json(1, "return")}),
        );
        assert_eq!(committed["outcome"], json!("no-op"));
        assert_eq!(committed["revision"], json!(1));
        assert_eq!(committed["created_entry_id"], json!(null));
        assert_eq!(entries(&mut service, &mut session), before, "no new entry");
        assert_eq!(
            ok(&mut service, &mut session, "session.state", json!({}))["draft"],
            json!(null),
            "a no-op ends the gesture like any other commit"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn an_external_commit_conflicts_a_draft_and_reapply_keeps_only_this_clients_fields() {
        let (mut service, catalog, asset) = patched("draft-conflict");
        let mut editor = ClientSession::default();
        let mut agent = ClientSession::default();
        let draft_id = ok(
            &mut service,
            &mut editor,
            "draft.begin",
            json!({"asset_id": asset, "action": PATCH_ACTION}),
        )["draft_id"]
            .clone();
        ok(
            &mut service,
            &mut editor,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"red": 40.0}}),
        );
        // Another client changes a field this gesture never touched.
        ok(
            &mut service,
            &mut agent,
            &patch_method(),
            json!({"asset_id": asset, "mutation": mutation_json(0, "agent"), "green": 60.0}),
        );
        assert!(
            agent.draft.is_none() && editor.draft.is_some(),
            "two clients' drafts never interact"
        );

        let read = ok(
            &mut service,
            &mut editor,
            "draft.read",
            json!({"draft_id": draft_id}),
        );
        assert_eq!(read["conflicted"], json!(true));
        assert_eq!(read["base_revision"], json!(0));
        assert_eq!(
            ok(&mut service, &mut editor, "session.state", json!({}))["draft"]["conflicted"],
            json!(true),
            "the session reports the conflict without any notification path"
        );
        let refused = call(
            &mut service,
            &mut editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation_json(0, "editor")}),
        )
        .error
        .expect("a conflicted draft cannot commit");
        assert_eq!(refused.code, "conflict");
        assert_eq!(
            ok(
                &mut service,
                &mut editor,
                "draft.read",
                json!({"draft_id": draft_id})
            )["fields"],
            json!({"red": 40.0}),
            "the refused draft is kept with its settings"
        );

        let reapplied = ok(
            &mut service,
            &mut editor,
            "draft.reapply",
            json!({"draft_id": draft_id}),
        );
        assert_eq!(reapplied["conflicted"], json!(false));
        assert_eq!(reapplied["base_revision"], json!(1));
        assert_eq!(
            reapplied["fields"],
            json!({"red": 40.0}),
            "reapply keeps only the fields this client set"
        );
        let committed = ok(
            &mut service,
            &mut editor,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation_json(1, "editor")}),
        );
        assert_eq!(committed["outcome"], json!("applied"));
        assert_eq!(
            described(&mut service, &mut editor, &asset)["layers"][0]["values"],
            json!({"red": 40.0, "green": 60.0}),
            "the other client's field survives this client's commit"
        );
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn a_draft_commit_checks_the_expected_revision_and_a_retried_request_is_deduplicated() {
        let (mut service, catalog, asset) = patched("draft-revision");
        let mut session = ClientSession::default();
        let draft_id = ok(
            &mut service,
            &mut session,
            "draft.begin",
            json!({"asset_id": asset, "action": PATCH_ACTION}),
        )["draft_id"]
            .clone();
        ok(
            &mut service,
            &mut session,
            "draft.set",
            json!({"draft_id": draft_id, "fields": {"red": 10.0}}),
        );
        let stale = call(
            &mut service,
            &mut session,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation_json(1, "stale")}),
        )
        .error
        .expect("the envelope must name the revision the draft was based on");
        assert_eq!(stale.code, "conflict");
        assert!(
            stale.message.contains("based on revision 0"),
            "{}",
            stale.message
        );
        assert!(
            session.draft.is_some(),
            "a refused commit keeps the gesture alive"
        );
        let committed = ok(
            &mut service,
            &mut session,
            "draft.commit",
            json!({"draft_id": draft_id, "mutation": mutation_json(0, "gesture")}),
        );
        assert_eq!(committed["outcome"], json!("applied"));

        // The commit is an ordinary action underneath, so retrying the identical request returns
        // the original result and writes no second entry.
        let retried = ok(
            &mut service,
            &mut session,
            &patch_method(),
            json!({"asset_id": asset, "mutation": mutation_json(0, "gesture"), "red": 10.0}),
        );
        assert_eq!(retried["deduplicated"], json!(true));
        assert_eq!(retried["current_entry_id"], committed["current_entry_id"]);
        assert_eq!(retried["revision"], committed["revision"]);
        let reused = call(
            &mut service,
            &mut session,
            &patch_method(),
            json!({"asset_id": asset, "mutation": mutation_json(0, "gesture"), "red": 11.0}),
        )
        .error
        .expect("the same request id with different input is a conflict");
        assert_eq!(reused.code, "conflict");
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
