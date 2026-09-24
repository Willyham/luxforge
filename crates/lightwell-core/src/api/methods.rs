//! The method table: every host method is one entry with its declared parameters, notes and
//! handler, and every module action, query and task resolves to a generated method from the same
//! registry, so discovery, event emission and dispatch cannot drift apart.
//!
//! A handler is either a service handler, which the editor service answers with the caller's
//! session, or an owner handler, which the catalog owner answers from its own state: its source and
//! analysis jobs, its event log, the capability host and the activity board. The catalog owner finds
//! a method here, calls its handler and records the event a change announces; nothing is routed any
//! other way.
use super::{
    COMPONENT_GALLERY_PAGE_COUNT, ClientSession, MASK_MODE, MaskOverlayColour, MaskOverlayMode,
    POINTER_MODE, PROTOCOL,
    owner::{self, Call, Owner},
    params::{self, Envelope, HostParams, NoParams, ParamSchema, host_params, parse},
};
use crate::{
    ActionDescriptor, ArtifactId, AssetId, ComponentId, DraftId, EditorService, EntryId, Error,
    ErrorKind, HistorySelection, MaskId, ModuleRegistry, Mutation, MutationOutcome, PresetId, Zoom,
    capabilities::{descriptor::TaskDescriptor, host::TASK_PREFIX},
    mask::commands::{self as mask_commands, MaskCommand, MaskTarget},
    path,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// A method the editor service answers with the caller's session.
pub(super) type ServiceHandler =
    fn(&mut EditorService, &mut ClientSession, &Value) -> Result<Value, Error>;

/// A method the catalog owner answers from its own state.
pub(super) type OwnerHandler = fn(&mut Owner, &Call<'_>) -> Result<Value, Error>;

pub(super) enum Handler {
    Service(ServiceHandler),
    Owner(OwnerHandler),
}

pub(super) struct MethodSpec {
    pub name: &'static str,
    /// The parameters, generated with the struct the handler parses.
    pub params: &'static ParamSchema,
    pub notes: &'static str,
    pub handler: Handler,
}

impl MethodSpec {
    /// A method mutates exactly when it carries a mutation envelope.
    pub(super) fn mutates(&self) -> bool {
        self.params.envelope != Envelope::None
    }
}

/// One service method: its handler takes the struct its schema was generated from, parsed from the
/// request by the one parse function, so the two cannot differ.
macro_rules! service {
    ($name:literal, $params:ty, $handler:path, $notes:expr $(,)?) => {
        MethodSpec {
            name: $name,
            params: &<$params as HostParams>::SCHEMA,
            notes: $notes,
            handler: Handler::Service(|service, session, request| {
                $handler(service, session, parse::<$params>(request)?)
            }),
        }
    };
}

/// One owner method, parsed the same way.
macro_rules! owner {
    ($name:literal, $params:ty, $handler:path, $notes:expr $(,)?) => {
        MethodSpec {
            name: $name,
            params: &<$params as HostParams>::SCHEMA,
            notes: $notes,
            handler: Handler::Owner(|owner, call| {
                $handler(owner, call, parse::<$params>(&call.request.params)?)
            }),
        }
    };
}

pub(super) const METHODS: &[MethodSpec] = &[
    service!(
        "schema.list",
        NoParams,
        schema_list,
        "protocol identity and every method with its parameters"
    ),
    owner!(
        "catalog.import",
        owner::Import,
        owner::catalog_import,
        "queues bounded source preparation; returns a job to inspect with job.status; commits only on verified success, which emits the event"
    ),
    owner!(
        "job.status",
        owner::JobParams,
        owner::job_status,
        "this client's bounded source job status (queued, running, ready, failed); ready includes the committed asset state"
    ),
    // The activity board belongs to the catalog owner, whose workers publish to it, so the owner
    // answers from it: one lock and a copy, nothing rendered or read.
    owner!(
        "activity.list",
        NoParams,
        owner::activity_list,
        "{sequence, active, recent, untracked}: the host's running work oldest first, and up to 16 recent entries that ran at least 250 ms, newest first; each entry has id, kind, label and elapsed_ms, or outcome (completed, cancelled or failed), duration_ms and ended_ms_ago, plus detail, asset_id, phase, progress {fraction, message} and job_id when known; job_id names the job that job.status (source work), analysis.read (histograms) or module.job.read (capability jobs) also answers, all with the shared status vocabulary (queued, running, ready, failed, cancelled, superseded); sequence changes exactly when the contents do; needs no asset, takes no parameters, mutates nothing and emits no event"
    ),
    owner!(
        "job.adopt",
        owner::JobParams,
        owner::job_adopt,
        "select the ready result of this client's latest import as current; stale imports are refused"
    ),
    owner!(
        "job.cancel",
        owner::JobParams,
        owner::job_cancel,
        "remove this client's interest in a source job without cancelling other clients"
    ),
    owner!(
        "source.prepare",
        owner::SourcePrepare,
        owner::source_prepare,
        "queue signature-verified preparation of an imported source after reopen or cache eviction"
    ),
    service!(
        "catalog.list",
        NoParams,
        catalog_list,
        "referenced assets in import order"
    ),
    service!(
        "asset.state",
        AssetParams,
        asset_state,
        "current entry, revision and redo path"
    ),
    service!(
        "source.inspect",
        SourceInspect,
        source_inspect,
        "persisted source identity, RAW interpretation, crop, backend and preparation readiness without decoding"
    ),
    service!(
        "history.list",
        HistoryList,
        history_list,
        "chronological entries newest first, including abandoned branches"
    ),
    service!(
        "history.inspect",
        EntryParams,
        history_inspect,
        "one entry with its complete immutable stack"
    ),
    service!(
        "history.lineage",
        HistoryLineage,
        history_lineage,
        "undo-parent chain newest first; next_entry_id continues a longer chain"
    ),
    service!(
        "recipe.describe",
        RecipeDescribe,
        recipe_describe,
        "an entry's stored layers in order with their module, title, summary and availability; reads payloads only and renders nothing"
    ),
    service!(
        "module.list",
        ModuleList,
        module_list,
        "every registered module descriptor with its effects, actions, parameters and controls"
    ),
    // Module settings are answered by the catalog owner, which holds the capability host: the
    // settings directory and the secret store. They are user-level, outside every catalog, and
    // never create history entries.
    owner!(
        "module.settings.read",
        owner::capability::ModuleParams,
        owner::capability::settings_read,
        "{module_id, schema, revision, state, fields, profiles}: each field's value, default, source (user or default) and validity, a secret field as {secret_present} only, and each profile's status (ready, incomplete, missing-credentials or incompatible); state is ready, incomplete or incompatible"
    ),
    owner!(
        "module.settings.set",
        owner::capability::SetParams,
        owner::capability::settings_set,
        "validates the named non-secret fields against their kinds and commits them together; null returns a field to its default; an endpoint is stored as the URL the transport policy accepts and a file as its canonical path; a secret field is refused; mutation.expected_revision is the module's settings revision; returns {outcome, revision, changed, invalidates_activation, settings}"
    ),
    owner!(
        "module.settings.set-secret",
        owner::capability::SetSecretParams,
        owner::capability::settings_set_secret,
        "stores one secret field's value in the secure store and never echoes it; a retry is matched by the setting alone; not-ready names a locked or unavailable store and nothing is kept in plain text"
    ),
    owner!(
        "module.settings.clear-secret",
        owner::capability::ClearSecretParams,
        owner::capability::settings_clear_secret,
        "removes only that secret from the secure store; an absent secret is a no-op"
    ),
    owner!(
        "module.settings.reset",
        owner::capability::ResetParams,
        owner::capability::settings_reset,
        "deletes the module's stored values and profiles and clears their secrets; the one write an incompatible entry accepts; the revision keeps counting"
    ),
    owner!(
        "module.profile.create",
        owner::capability::CreateProfileParams,
        owner::capability::profile_create,
        "a new empty provider profile of a declared adapter with a host-generated profile-<uuid> identity, at most the module's declared maximum; returns it as profile"
    ),
    owner!(
        "module.profile.remove",
        owner::capability::RemoveProfileParams,
        owner::capability::profile_remove,
        "clears the profile's secrets and removes it and its values, revokes the profile's grants and returns the removed profile"
    ),
    // Permissions, activation, resources and capability jobs are answered by the catalog owner
    // too: grants live beside the settings, and the jobs, lanes and activation state live there.
    owner!(
        "module.permission.grant",
        owner::capability::GrantParams,
        owner::capability::permission_grant,
        "grants one exact scope of a declared capability; only a client with permission authority may, otherwise forbidden; scope is {resource, version, origin} for download-artifact (the declared version from the origin of its pinned URL) or {profile_id, adapter, origin, data, asset_id} for remote-image-request (an existing profile of that adapter whose endpoint has that origin, the capability's data class and an asset of this catalog); clears a matching denial; a scope that already has a live grant returns it as a no-op; returns {grant, outcome, deduplicated}"
    ),
    owner!(
        "module.permission.deny",
        owner::capability::DenyParams,
        owner::capability::permission_deny,
        "records that the person did not allow one exact scope; the next consent-required for it reports denied: true; any client may; returns {denial, deduplicated}"
    ),
    owner!(
        "module.permission.revoke",
        owner::capability::RevokeParams,
        owner::capability::permission_revoke,
        "marks the grant revoked and cancels the queued and running jobs that depend on it with cancelled: permission revoked; never touches recipes, history or artifacts; any client may; returns {grant, outcome, cancelled_jobs, deduplicated}"
    ),
    owner!(
        "module.permission.list",
        owner::capability::PermissionList,
        owner::capability::permission_list,
        "{grants, denials}: every grant, revoked ones with {revoked: {ms, reason}}, and every recorded denial; none holds a secret"
    ),
    owner!(
        "module.activate",
        owner::capability::ModuleChange,
        owner::capability::module_activate,
        "checks the module's declared required settings and resources and fails with not-ready and data.requirements [{kind, id, state}] listing every missing one before anything is queued; otherwise queues its activation on the module lane, or joins the one queued or running; returns {module_id, activation, job_id?, status?, deduplicated}; an active module answers activation: active with no job"
    ),
    owner!(
        "module.deactivate",
        owner::capability::ModuleChange,
        owner::capability::module_deactivate,
        "supersedes a queued activation, cancels a running one, or marks an active module inactive and queues the release of what it loaded after the module lane's earlier work; never deletes a resource or an edit; returns {module_id, activation, job_id?, status?, deduplicated}"
    ),
    owner!(
        "module.status",
        owner::capability::ModuleParams,
        owner::capability::module_status,
        "{module_id, activation: {state, reason?, job_id?, error?}, settings: {state, revision, missing}, resources, permissions: {grants, denials}, jobs}; state is inactive, activating, active or failed; reads settings, stats installed markers and reads grants, and loads nothing"
    ),
    owner!(
        "module.resource.list",
        owner::capability::ModuleParams,
        owner::capability::resource_list,
        "{resources: [{id, title, version, bytes, sha256, license, provenance, url, state, path?, installed_ms?, job_id?, error?}], storage: {root, used_bytes, quota_bytes}}; state is not-installed, installing, installed or failed; stats only, no hashing"
    ),
    owner!(
        "module.resource.install",
        owner::capability::InstallParams,
        owner::capability::resource_install,
        "queues a transfer-lane job that streams into staging, checks the pinned length and SHA-256, asks the module to check the format, checks the storage quota and only then installs; consent-required and resource-limit are reported before anything is queued; an installed resource answers state: installed and a second request joins the running install; returns {module_id, resource_id, state, job_id?, status?, deduplicated}"
    ),
    owner!(
        "module.resource.remove",
        owner::capability::ResourceParams,
        owner::capability::resource_remove,
        "queues a transfer-lane job that deletes the installed version; a module that requires it and is active or activating is deactivated first; never touches a catalog, recipe or artifact; returns {module_id, resource_id, state, job_id?, status?, deduplicated}"
    ),
    owner!(
        "module.job.read",
        owner::capability::JobParams,
        owner::capability::job_read,
        "{job_id, kind, module_id, resource_id?, status, progress: {fraction?, message?}, result?, error?: {code, message, data?}, request_id?}; kind is activate, deactivate, install, remove or task; status is queued, running, ready, failed, cancelled or superseded; any client may read any capability job; the owner keeps the last 32 finished"
    ),
    owner!(
        "module.job.cancel",
        owner::capability::JobCancelParams,
        owner::capability::job_cancel,
        "removes a queued job as cancelled, or asks a running one to stop at its next checkpoint; a finished job is returned unchanged; a deactivation cannot be cancelled; any client may; returns the job with deduplicated"
    ),
    service!(
        "history.undo",
        Navigate,
        history_undo,
        "moves current to its undo parent without adding an entry"
    ),
    service!(
        "history.redo",
        Navigate,
        history_redo,
        "follows the persisted redo path"
    ),
    service!(
        "history.restore",
        Restore,
        history_restore,
        "appends a restore action copying the entry's stack and returns the session to current"
    ),
    service!(
        "version.create",
        VersionCreate,
        version_create,
        "names a retained entry; unique per asset ignoring case; no-op when the name already names that entry; records mutation.actor"
    ),
    service!(
        "version.delete",
        VersionDelete,
        version_delete,
        "removes the name only; the entry stays in history; no-op when absent"
    ),
    service!(
        "version.list",
        AssetParams,
        version_list,
        "saved versions in creation order with their entry sequence"
    ),
    // The preset library is catalog data beside history. None of these methods renders, opens a
    // source or hashes pixels; applying a preset is edit.apply-preset.
    service!(
        "preset.list",
        NoParams,
        preset_list,
        "{presets: [{id, name, group, settings, origin, report, actor, created_ms, updated_ms, unavailable}]} sorted by group, then name, ignoring case; report is the import report's counts {mapped, neutral, unsupported, refused}, or null for a preset created in Lightwell; unavailable names the settings actions this registry cannot apply; no source_text"
    ),
    service!(
        "preset.read",
        PresetParams,
        preset_read,
        "{preset}: one record with its full import report and source_text, the imported file's text kept verbatim, or null for a preset created in Lightwell"
    ),
    service!(
        "preset.create",
        PresetCreate,
        preset_create,
        "{preset, deduplicated}: stores a settings set as a Lightwell preset with origin {kind: lightwell}, report null and mutation.actor as its actor; name is 1..128 and group 1..64 printable characters after trimming, the (group, name) pair is unique ignoring case (a duplicate is a conflict) and the library holds at most 1000 presets (resource-limit); every action and field is checked against the registry"
    ),
    service!(
        "preset.capture",
        PresetCapture,
        preset_capture,
        "{settings} read from one entry's stack: fields maps field-patch actions to an array of their parameter names or true for all of them; each field takes the value of its module's one layer, or its declared default when the stack has none; two or more layers are validation: ambiguous; reads stored payloads only, so it opens no source and renders nothing; send the result to preset.create"
    ),
    service!(
        "preset.update",
        PresetUpdate,
        preset_update,
        "{outcome, preset, deduplicated}: applied when the name, group or settings change, recording mutation.actor and updated_ms; no-op, with nothing written, when they do not; a rename onto another preset's (group, name) pair is a conflict; origin and report are kept"
    ),
    service!(
        "preset.delete",
        PresetDelete,
        preset_delete,
        "{outcome, deleted, deduplicated}: applied and true when the preset existed, no-op and false when it is absent; history entries that applied it are unchanged"
    ),
    service!(
        "preset.export",
        PresetParams,
        preset_export,
        "{file_name, content}: the preset as a Lightwell preset document named <name>.lwpreset, which preset.import reads back to the same name, group and settings"
    ),
    service!(
        "preset.inspect",
        PresetInspect,
        preset_inspect,
        "dry run of preset.import that stores nothing: {preset, report}, where preset has the record's shape with id, actor, created_ms and updated_ms null, the file's name and the file's group or Imported, and report is the full per-setting import report; a file that maps nothing still returns its report with empty settings"
    ),
    service!(
        "preset.import",
        PresetImport,
        preset_import,
        "{preset, report, deduplicated}: reads the text of a Lightwell preset document, a Lightroom XMP preset or a .lrtemplate, at most 1 MiB, and stores its mapped settings with the text kept verbatim and mutation.actor as its actor; the library's name, uniqueness and size rules apply as for preset.create; a file that maps nothing is unsupported-input with the report counts, and a refused import stores nothing"
    ),
    service!(
        "preview.select",
        EntryParams,
        preview_select,
        "read-only session selection; the current entry selects current, not a historical preview; returns generation and session"
    ),
    service!(
        "preview.return-current",
        NoParams,
        preview_return_current,
        "returns generation and session"
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
        "per-client screen preference: panels, canvas mode, overlays and diagnostic components page; needs no asset and changes no history or frame; returns the session"
    ),
    service!(
        "session.state",
        NoParams,
        session_state,
        "this client's selection, view, workspace state and session revision"
    ),
    service!(
        "resources.read",
        NoParams,
        resources_read,
        "what the operating system accounts to this process: {monotonic_ns, cpu: {time_ns, logical_cpus}, memory: {kind, bytes, peak_bytes, resident_bytes}, gpu: {time_ns, allocated_bytes, unified_memory}, budgets: {colour_scratch, spatial} each {target_bytes, in_use_bytes, peak_bytes}}; times are nanoseconds and sizes bytes; memory.kind is footprint (macOS, Activity Monitor's Memory, including GPU allocations on unified memory), resident (Linux) or private (Windows); time counters are cumulative, so a rate comes from two reads: CPU percent of one core is 100 × Δcpu.time_ns / Δmonotonic_ns, up to 100 × logical_cpus, and GPU percent is the same over gpu.time_ns; monotonic_ns means something only as a difference; a counter the platform cannot give is omitted and its object's unavailable maps its key to the reason; takes no parameters, needs no asset, emits no event and changes nothing"
    ),
    service!(
        "draft.begin",
        DraftBegin,
        draft_begin,
        "opens this client's one draft of that action, bound to the asset's current revision; refused while a draft is open or a historical entry is previewed"
    ),
    service!(
        "draft.set",
        DraftSet,
        draft_set,
        "validates the named fields against the action's parameters and merges them into the draft; an invalid field changes nothing"
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
    service!(
        "draft.commit",
        DraftCommit,
        draft_commit,
        "runs the draft's action with its accumulated fields and ends the draft; a conflicted draft or a mismatched expected_revision is refused and the draft is kept"
    ),
    service!(
        "draft.reapply",
        DraftParams,
        draft_reapply,
        "rebases the draft on the asset's current revision, keeping and revalidating only the fields this client set"
    ),
    service!(
        "render.sample",
        RenderSample,
        render_sample,
        "one pixel of the session's selected entry, or of an open draft's effective recipe, evaluated without rasterizing"
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
    // The analysis methods are answered by the catalog owner, because the job store, the worker
    // slots and every client's draft live there. They mutate nothing and emit no event.
    owner!(
        "analysis.request",
        owner::AnalysisRequest,
        owner::analysis_request,
        "queues the exact RGB histogram and output-clipping reduction of one evaluated stack and returns {job_id, status, identity} promptly, with the report included when the store already holds it; target is {kind:current}, {kind:entry,entry_id} or {kind:draft,draft_id} for this client's own draft; identical identities share one job"
    ),
    owner!(
        "analysis.read",
        owner::AnalysisJobParams,
        owner::analysis_read,
        "{status, identity, report?, error?}; status is queued, running, ready, failed, superseded or cancelled and only ready carries counts, so no status can be read as an empty histogram; a job this client did not request is a validation error"
    ),
    owner!(
        "analysis.cancel",
        owner::AnalysisJobParams,
        owner::analysis_cancel,
        "drops this client's interest in the job and cancels the work only when no other client holds it; returns {cancelled: true}"
    ),
    service!(
        "artifact.status",
        NoParams,
        artifact_status,
        "the catalog's derived-artifact root and its state (absent, ready, missing or foreign), the catalog_id its manifest must name, and how many artifacts entries reference, how many a collection would remove and their recorded bytes; reads the manifest and counts rows only"
    ),
    service!(
        "artifact.inspect",
        ArtifactInspect,
        artifact_inspect,
        "one artifact's record (hash, bytes, kind, dimensions, colour, publishing module, time), whether its file is present, missing or of the wrong length, how many entries reference it and whether a task of this process published it; stats only"
    ),
    // Collection runs on the source worker and is read with job.status, so the catalog owner
    // answers it. It emits its event when the request is accepted.
    owner!(
        "artifact.collect",
        owner::Collect,
        owner::artifact_collect,
        "removes the rows of artifacts no entry references and no task of this process published, then queues a source job that removes their files, object files without a row and staged files older than an hour; nothing an entry references is touched; returns {job_id, status, deduplicated} and the job result counts {rows, objects, temporary}"
    ),
];

/// A resolved method: a host method from the static table, or one generated from a registered
/// module action, query or task. All of them come from the same lookup discovery uses.
pub(super) enum Method {
    Host(&'static MethodSpec),
    Action(String),
    /// A module's read-only query. It writes nothing, so it never emits an event.
    Query(String),
    /// A host command of the `mask.*` family, declared with the same descriptor types a module
    /// action uses and dispatched through the same lookup.
    Mask(&'static MaskCommand),
    /// A module's worker task. The request queues a capability job and changes nothing itself; the
    /// catalog owner answers it and announces the task when it succeeds.
    Task,
}

/// Who answers a resolved method.
pub(super) enum Route {
    /// The editor service, with the caller's session: [`Method::serve`].
    Service,
    Owner(OwnerHandler),
}

impl Method {
    /// The mutation envelope the method carries. A module action and a mutating mask command change
    /// an asset, which has a revision.
    pub(super) fn envelope(&self) -> Envelope {
        match self {
            Self::Host(spec) => spec.params.envelope,
            Self::Action(_) => Envelope::Revision,
            Self::Mask(command) if command.mutates => Envelope::Revision,
            Self::Mask(_) | Self::Query(_) | Self::Task => Envelope::None,
        }
    }

    pub(super) fn mutates(&self) -> bool {
        self.envelope() != Envelope::None
    }

    pub(super) fn route(&self) -> Route {
        match self {
            Self::Host(MethodSpec {
                handler: Handler::Owner(handler),
                ..
            }) => Route::Owner(*handler),
            Self::Task => Route::Owner(owner::capability::task),
            Self::Host(_) | Self::Action(_) | Self::Query(_) | Self::Mask(_) => Route::Service,
        }
    }

    /// Answer a method the editor service answers. The owner never sends it one of its own, which
    /// [`Method::route`] names.
    pub(super) fn serve(
        &self,
        service: &mut EditorService,
        session: &mut ClientSession,
        params: &Value,
    ) -> Result<Value, Error> {
        match self {
            Self::Host(MethodSpec {
                handler: Handler::Service(handler),
                ..
            }) => handler(service, session, params),
            Self::Action(action_id) => edit_action(service, session, action_id, params),
            Self::Query(query_id) => module_query(service, session, query_id, params),
            Self::Mask(command) => mask_command(service, session, command, params),
            Self::Host(MethodSpec {
                name,
                handler: Handler::Owner(_),
                ..
            }) => Err(owner_answered(name)),
            Self::Task => Err(owner_answered("a task")),
        }
    }
}

fn owner_answered(name: &str) -> Error {
    Error::new(
        ErrorKind::Internal,
        format!("{name} is answered by the catalog owner"),
    )
}

/// Action method names are generated: action `set-pixel` is `edit.set-pixel`.
pub(super) fn action_method(action_id: &str) -> String {
    format!("edit.{action_id}")
}

/// Query method names are generated the same way in their own namespace: query `neutral-sample` is
/// `query.neutral-sample`.
pub(super) fn query_method(query_id: &str) -> String {
    format!("query.{query_id}")
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
    if let Some(command) = mask_commands::find(name) {
        return Some(Method::Mask(command));
    }
    if let Some(action_id) = name.strip_prefix("edit.") {
        return service
            .registry()
            .action(action_id)
            .map(|_| Method::Action(action_id.to_owned()));
    }
    if let Some(task_id) = name.strip_prefix(TASK_PREFIX) {
        return service.registry().task(task_id).map(|_| Method::Task);
    }
    let query_id = name.strip_prefix("query.")?;
    service
        .registry()
        .query(query_id)
        .map(|_| Method::Query(query_id.to_owned()))
}

/// The envelope of a host method by name, for test clients that fill it in.
#[cfg(test)]
pub(crate) fn host_envelope(name: &str) -> Envelope {
    METHODS
        .iter()
        .find(|spec| spec.name == name)
        .map_or(Envelope::None, |spec| spec.params.envelope)
}

/// A method emits an event when it is mutating and its result changed something: not a no-op, and
/// not a retry answered from a request log, which keeps the original `outcome` but reports
/// `deduplicated` because the first attempt already emitted the event.
pub(super) fn mutates(method: &Method, result: Option<&Value>) -> bool {
    method.mutates()
        && result.is_none_or(|value| {
            value.get("outcome").and_then(Value::as_str) != Some("no-op")
                && value.get("deduplicated").and_then(Value::as_bool) != Some(true)
        })
}

/// One host method's schema entry, generated from its declared parameters.
fn host_schema(spec: &MethodSpec) -> Value {
    let optional: Map<String, Value> = spec
        .params
        .optional
        .iter()
        .map(|(name, meaning)| ((*name).to_owned(), json!(meaning)))
        .collect();
    let mut schema = json!({
        "mutates": spec.mutates(),
        "required": spec.params.required,
        "optional": optional,
        "notes": spec.notes,
    });
    if let Some(envelope) = spec.params.envelope.name() {
        schema["mutation"] = json!(envelope);
    }
    schema
}

/// One generated method description: the envelope every action shares plus the action's own
/// declared parameters, so a client needs no hand-maintained list.
///
/// `maskable` says whether this action belongs to a module that declares a maskable effect, in which
/// case the host's one optional `mask` field is listed with the envelope. It is listed here rather
/// than declared as a parameter because it is the host's and no module parses it: sending it to any
/// other action is a validation error naming that action.
fn action_schema(action: &ActionDescriptor, maskable: bool) -> Value {
    let mut required = vec![json!("asset_id"), json!("mutation")];
    let mut optional = Map::new();
    if maskable {
        optional.insert(
            "mask".to_owned(),
            json!(
                "the mask this edit applies through; omit it to edit the layer that applies \
                 everywhere. The global layer and each mask are distinct targets, so this action \
                 commits or updates one layer per target"
            ),
        );
    }
    for parameter in &action.parameters {
        // A patch carries whichever fields the caller names, so none of them is required however
        // the parameter is declared; its default is what a client seeds or resets the field to.
        if !action.patch && parameter.required && parameter.default.is_none() {
            required.push(json!(parameter.name));
        } else {
            optional.insert(parameter.name.clone(), json!(parameter.notes));
        }
    }
    json!({
        "mutates": true,
        "mutation": Envelope::Revision.name(),
        "patch": action.patch,
        "required": required,
        "optional": optional,
        "notes": action.notes,
        "parameters": action.parameters,
    })
}

/// One generated query description. `asset_id` is the envelope, `entry_id` selects the stack to ask
/// about and defaults to the session's selection, and the remaining top-level fields are the
/// query's own declared parameters. A query mutates nothing.
fn query_schema(query: &ActionDescriptor) -> Value {
    let mut required = vec![json!("asset_id")];
    let mut optional = Map::new();
    optional.insert(
        "entry_id".to_owned(),
        json!("entry to ask about; default the session's selection"),
    );
    for parameter in &query.parameters {
        if parameter.required && parameter.default.is_none() {
            required.push(json!(parameter.name));
        } else {
            optional.insert(parameter.name.clone(), json!(parameter.notes));
        }
    }
    json!({
        "mutates": false,
        "required": required,
        "optional": optional,
        "notes": query.notes,
        "parameters": query.parameters,
    })
}

/// One generated task description. `asset_id` and `profile_id` are the envelope when the task
/// declares them, and the remaining top-level fields are its own declared parameters. The request
/// itself changes nothing: it queues a task job and answers `{job_id, status}`.
fn task_schema(task: &TaskDescriptor) -> Value {
    let mut required = Vec::new();
    if task.asset {
        required.push(json!("asset_id"));
    }
    if task.profile {
        required.push(json!("profile_id"));
    }
    let mut optional = Map::new();
    for parameter in &task.parameters {
        if parameter.required && parameter.default.is_none() {
            required.push(json!(parameter.name));
        } else {
            optional.insert(parameter.name.clone(), json!(parameter.notes));
        }
    }
    json!({
        "mutates": false,
        "required": required,
        "optional": optional,
        "notes": format!(
            "{} Checks the task's requirements (not-ready with data.requirements) and a live grant for each capability it uses (consent-required naming the first missing one) before anything is queued, then queues a task job on the module lane; returns {{job_id, status}}; module.job.read reports {{result, artifacts}} when it succeeds",
            task.notes
        ),
        "parameters": task.parameters,
    })
}

pub fn schemas(registry: &ModuleRegistry) -> Value {
    let mut methods: Map<String, Value> = METHODS
        .iter()
        .map(|spec| (spec.name.to_owned(), host_schema(spec)))
        .collect();
    // The host's own `mask.*` family, declared from the same descriptor types, so a client
    // discovers a mask command and a module action from one listing.
    for command in mask_commands::all() {
        let mut schema = command.schema();
        if let Some(envelope) = Method::Mask(command).envelope().name() {
            schema["mutation"] = json!(envelope);
        }
        methods.insert(command.method.to_owned(), schema);
    }
    let descriptors = registry.descriptors();
    for descriptor in &descriptors {
        // One maskable effect makes this module's actions carry the target field; the registry
        // answers the same question the same way for dispatch.
        let maskable = descriptor.effects.iter().any(|effect| effect.maskable);
        for action in &descriptor.actions {
            methods.insert(action_method(&action.id), action_schema(action, maskable));
        }
        for query in &descriptor.queries {
            methods.insert(query_method(&query.id), query_schema(query));
        }
        for task in &descriptor.tasks {
            methods.insert(task_method(&task.id), task_schema(task));
        }
    }
    json!({
        "protocol": PROTOCOL,
        "coordinate_space": "Each edit uses integer coordinates in its own input image stage after EXIF orientation. A pixel or colour edit addresses the content stage, the source after EXIF orientation, because the host places both before the quarter-turns, reflections and crop that carry them; a colour edit addresses every pixel of that stage and changes no dimension. A number parameter carries a finite JSON number within its declared range, such as an angle in degrees or a rectangle normalized to its stage; a JSON integer is accepted and passed through unchanged.",
        "methods": methods,
        "modules": descriptors,
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
            "decimation_tolerance": path::DECIMATION_TOLERANCE,
            "stored_deviation": path::STORED_DEVIATION,
            "points_per_stroke": path::POINTS_PER_STROKE,
            "notes": "A posted path is snapped to a grid of stored_steps_per_unit steps per unit and decimated on that grid at decimation_tolerance, so a stored position is at most stored_deviation from the position that was posted and the same posted path always produces the same stored stroke. Decimation is idempotent: a desktop decimates before it posts, and a path posted undecimated arrives at the same stored bytes. A stroke is stored once under the hash of its contents and an entry's recipe references it by that hash, so a payload's strokes field holds addresses and never positions.",
        },
        // The host's mask surface: the widgets of the `mask.*` commands, in the same `Control`
        // vocabulary a module declares, so a client renders a mask's fields, toggles and mode
        // selector with the widgets it already has and invents no operation of its own.
        "masks": {
            "controls": mask_commands::controls(),
            // The one path bound that is the mask's own rather than the host path primitive's, so
            // the `paths` block above says nothing about masks and this one says what a mask adds.
            "points_per_mask": crate::POINTS_PER_MASK,
        },
        // Every mutating method names its envelope in its own `mutation` field.
        "mutation": {
            "revision": Envelope::Revision.fields(),
            "request": Envelope::Request.fields(),
            "notes": "Every mutating method carries a mutation envelope. A method that changes an asset or a module's settings, which have a revision, carries revision: expected_revision must be that revision or the request is a conflict. Every other mutating method carries request. request_id and actor are 1..128 characters. A retry with the same request_id and the same input returns the first answer, marked deduplicated: true, and emits no event; the same request_id with different input is a conflict. A revision's request_id is unique per asset, or per module for settings, and is remembered durably with the change; a request's is unique per method family, the method name without its last segment, and is remembered for the owner's lifetime, bounded to the most recent requests.",
        },
    })
}

host_params! {
    pub(super) struct AssetParams {
        asset_id: AssetId,
    }
}

host_params! {
    pub(super) struct EntryParams {
        asset_id: AssetId,
        entry_id: EntryId,
    }
}

host_params! {
    pub(super) struct SourceInspect {
        asset_id: AssetId,
        entry_id: Option<EntryId> = "historical entry; default current",
    }
}

host_params! {
    pub(super) struct HistoryList {
        asset_id: AssetId,
        before_sequence: Option<u64> = "u64",
        limit: Option<usize> = "1..100",
    }
}

host_params! {
    pub(super) struct HistoryLineage {
        asset_id: AssetId,
        entry_id: Option<EntryId> = "start entry; default current",
        limit: Option<usize> = "1..100",
    }
}

host_params! {
    pub(super) struct RecipeDescribe {
        asset_id: AssetId,
        entry_id: Option<EntryId> = "entry to describe; default current",
    }
}

host_params! {
    pub(super) struct ModuleList {
        asset_id: Option<AssetId> = "filter controls for this asset source kind",
    }
}

host_params! {
    pub(super) struct Navigate {
        asset_id: AssetId,
        mutation: Mutation,
    }
}

host_params! {
    pub(super) struct Restore {
        asset_id: AssetId,
        mutation: Mutation,
        entry_id: EntryId,
    }
}

host_params! {
    pub(super) struct VersionCreate {
        asset_id: AssetId,
        name: String,
        mutation: MutationRequest,
        entry_id: Option<EntryId> = "entry to name; default current",
    }
}

host_params! {
    pub(super) struct VersionDelete {
        asset_id: AssetId,
        name: String,
        mutation: MutationRequest,
    }
}

host_params! {
    pub(super) struct PresetParams {
        preset_id: PresetId,
    }
}

host_params! {
    pub(super) struct PresetCreate {
        name: String,
        settings: Map<String, Value>,
        mutation: MutationRequest,
        group: Option<String> = "1..64 printable characters; default User presets",
    }
}

host_params! {
    pub(super) struct PresetCapture {
        asset_id: AssetId,
        fields: Map<String, Value>,
        entry_id: Option<EntryId> = "entry to read; default the session's selection",
    }
}

host_params! {
    pub(super) struct PresetUpdate {
        preset_id: PresetId,
        mutation: MutationRequest,
        name: Option<String> = "1..128 printable characters",
        group: Option<String> = "1..64 printable characters",
        settings: Option<Map<String, Value>> = "a settings set checked against the registry",
    }
}

host_params! {
    pub(super) struct PresetDelete {
        preset_id: PresetId,
        mutation: MutationRequest,
    }
}

host_params! {
    pub(super) struct PresetInspect {
        content: String,
        file_name: Option<String> = "the file's name, for the fallback preset name and the origin",
    }
}

host_params! {
    pub(super) struct PresetImport {
        content: String,
        mutation: MutationRequest,
        file_name: Option<String> = "the file's name, for the fallback preset name and the origin",
        name: Option<String> = "overrides the file's name",
        group: Option<String> = "overrides the file's group; default Imported",
    }
}

host_params! {
    pub(super) struct ViewSet {
        zoom: Option<Zoom> = "{mode:fit} or {mode:percent,value:10..1600}",
        pan_x: Option<f32> = "finite",
        pan_y: Option<f32> = "finite",
    }
}

/// A normal `Option<Option<T>>` deserializer cannot distinguish a missing field from explicit
/// JSON null. `workspace.set` needs that distinction: omission preserves, null closes the board.
fn present_nullable_page<'de, D>(deserializer: D) -> Result<Option<Option<usize>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<usize>::deserialize(deserializer).map(Some)
}

host_params! {
    pub(super) struct WorkspaceSet {
        state_panel: Option<bool> = "bool",
        tools_panel: Option<bool> = "bool",
        mode: Option<String> = "pointer or an available module id that declares a canvas interaction",
        thirds: Option<bool> = "bool",
        clip_shadows: Option<bool> = "bool; show the shadow clipping overlay",
        clip_highlights: Option<bool> = "bool; show the highlight clipping overlay",
        // Taken as strings so an unknown one is refused with the vocabulary spelled out, as `mode`
        // is, rather than with serde's report of an unmatched variant.
        mask_overlay: Option<String> = "off, tint, mask-on-black or image-on-black; what the canvas draws of the selected mask",
        mask_overlay_colour: Option<String> = "green or white; the tint the mask overlay is drawn in",
        #[serde(default, deserialize_with = "present_nullable_page")]
        component_gallery: Option<Option<usize>> = "null closes the diagnostic components board; integer 0..9 selects a page",
    }
}

host_params! {
    pub(super) struct DraftBegin {
        asset_id: AssetId,
        action: String,
        /// The mask and component a `mask.*` gesture edits. A module action's draft takes neither.
        mask: Option<MaskId> = "the mask a mask.* gesture edits, or the mask an action of a maskable effect drafts through",
        component: Option<ComponentId> = "the component inside that mask, for a mask.* gesture only",
    }
}

host_params! {
    pub(super) struct DraftSet {
        draft_id: DraftId,
        fields: Map<String, Value>,
    }
}

host_params! {
    pub(super) struct DraftParams {
        draft_id: DraftId,
    }
}

host_params! {
    pub(super) struct DraftCommit {
        draft_id: DraftId,
        mutation: Mutation,
    }
}

host_params! {
    pub(super) struct RenderSample {
        asset_id: AssetId,
        x: u32,
        y: u32,
        draft_id: Option<DraftId> = "this client's draft to sample instead of the stored stack",
    }
}

host_params! {
    pub(super) struct RenderLocate {
        asset_id: AssetId,
        x: u32,
        y: u32,
        entry_id: Option<EntryId> = "entry to locate in; default the session's selection",
    }
}

host_params! {
    pub(super) struct RenderTransform {
        asset_id: AssetId,
        entry_id: Option<EntryId> = "entry to answer for; default the session's selection",
    }
}

host_params! {
    pub(super) struct ArtifactInspect {
        artifact_id: ArtifactId,
    }
}

fn schema_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    _: NoParams,
) -> Result<Value, Error> {
    Ok(schemas(service.registry()))
}

fn module_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: ModuleList,
) -> Result<Value, Error> {
    let raw = p
        .asset_id
        .as_ref()
        .map(|id| service.state(id))
        .transpose()?
        .is_some_and(|state| matches!(state.asset.source, crate::SourceKind::Raw { .. }));
    let modules = service
        .registry()
        .descriptors()
        .into_iter()
        .filter(|module| p.asset_id.is_none() || module.id != "lightwell.raw" || raw)
        .collect::<Vec<_>>();
    Ok(json!({"modules": modules}))
}

fn catalog_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    _: NoParams,
) -> Result<Value, Error> {
    Ok(json!({"assets": service.assets()?}))
}

fn asset_state(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: AssetParams,
) -> Result<Value, Error> {
    value(service.state(&p.asset_id)?)
}

fn source_inspect(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: SourceInspect,
) -> Result<Value, Error> {
    service.inspect_source(&p.asset_id, p.entry_id.as_ref())
}

fn history_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: HistoryList,
) -> Result<Value, Error> {
    value(service.history(&p.asset_id, p.before_sequence, p.limit.unwrap_or(50))?)
}

fn history_inspect(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: EntryParams,
) -> Result<Value, Error> {
    value(service.entry(&p.asset_id, &p.entry_id)?)
}

fn history_lineage(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: HistoryLineage,
) -> Result<Value, Error> {
    value(service.lineage(&p.asset_id, p.entry_id.as_ref(), p.limit.unwrap_or(50))?)
}

/// Every generated action method: `asset_id` and `mutation` are the envelope, the remaining
/// top-level fields are the action's declared parameters.
fn edit_action(
    service: &mut EditorService,
    session: &mut ClientSession,
    action_id: &str,
    request: &Value,
) -> Result<Value, Error> {
    require_current(session)?;
    let mut parameters = params::generated(request)?;
    let asset_id: AssetId = params::take(&mut parameters, "asset_id")?;
    let mutation: Mutation = params::take(&mut parameters, "mutation")?;
    value(service.apply_action(&asset_id, mutation, action_id, Value::Object(parameters))?)
}

/// Every generated query method: `asset_id` is the envelope, `entry_id` names the stack to ask
/// about and defaults to the session's selection exactly as `render.sample` does, and the remaining
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

/// Every `mask.*` method: `asset_id` and `mutation` are the envelope a mutation always carries, and
/// `mask`, `component` and `name` join it because no declared parameter kind can carry an identity or
/// a person's free text. The remaining top-level fields are the command's own declared parameters and
/// go through the same generic check a module action's do.
fn mask_command(
    service: &mut EditorService,
    session: &mut ClientSession,
    command: &'static MaskCommand,
    request: &Value,
) -> Result<Value, Error> {
    let mut parameters = params::generated(request)?;
    let asset_id: AssetId = params::take(&mut parameters, "asset_id")?;
    // The one read-only command resolves its entry exactly as `render.sample` and `render.locate` do
    // and touches nothing at all.
    if command.method == mask_commands::LIST {
        let entry_id: Option<EntryId> = params::take_optional(&mut parameters, "entry_id")?;
        if let Some(name) = parameters.keys().next() {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("unknown field {name} for {}", command.method),
            ));
        }
        let entry_id = selected_entry(service, session, &asset_id, entry_id)?;
        return value(service.mask_listing(&asset_id, &entry_id)?);
    }
    // The other read: the pixel the operation one mask modulates receives. It resolves its entry the
    // way `mask.list` does, takes the mask in the envelope like every other command, and its two
    // coordinates through the same generic check a mutation's parameters take.
    if command.method == mask_commands::SAMPLE_INPUT {
        let entry_id: Option<EntryId> = params::take_optional(&mut parameters, "entry_id")?;
        let mask = params::take_optional(&mut parameters, "mask")?;
        let target = MaskTarget {
            mask,
            ..MaskTarget::default()
        };
        command.checked_target(&target)?;
        let checked = crate::check_parameters(&command.action, &Value::Object(parameters))?;
        let coordinate = |name: &str| -> Result<u32, Error> {
            u32::try_from(
                checked
                    .get(name)
                    .and_then(Value::as_i64)
                    .ok_or_else(|| missing_parameter(name, command.method))?,
            )
            .map_err(|_| missing_parameter(name, command.method))
        };
        let (x, y) = (coordinate("x")?, coordinate("y")?);
        let entry_id = selected_entry(service, session, &asset_id, entry_id)?;
        let mask = target
            .mask
            .as_ref()
            .expect("checked_target requires a mask");
        return value(service.mask_input_sample(&asset_id, &entry_id, mask, x, y)?);
    }
    require_current(session)?;
    let mutation: Mutation = params::take(&mut parameters, "mutation")?;
    let target = MaskTarget {
        mask: params::take_optional(&mut parameters, "mask")?,
        component: params::take_optional(&mut parameters, "component")?,
        name: params::take_optional(&mut parameters, "name")?,
        // A stroke's content address is an identity like the other two and travels here for the
        // same reason: no declared parameter kind carries one.
        stroke: params::take_optional(&mut parameters, "stroke")?,
    };
    value(service.apply_mask_command(
        &asset_id,
        mutation,
        command,
        Value::Object(parameters),
        target,
    )?)
}

/// A declared parameter a request did not carry, in the spelling the generic check already uses.
fn missing_parameter(name: &str, method: &str) -> Error {
    Error::new(
        ErrorKind::Validation,
        format!("missing required parameter {name} for action {method}"),
    )
}

fn history_undo(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: Navigate,
) -> Result<Value, Error> {
    require_current(session)?;
    value(service.undo(&p.asset_id, p.mutation)?)
}

fn history_redo(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: Navigate,
) -> Result<Value, Error> {
    require_current(session)?;
    value(service.redo(&p.asset_id, p.mutation)?)
}

fn history_restore(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: Restore,
) -> Result<Value, Error> {
    let result = service.restore(&p.asset_id, p.mutation, &p.entry_id)?;
    if !session.preview.can_edit() {
        session.preview.return_current();
        session.touch();
    }
    value(result)
}

fn version_create(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: VersionCreate,
) -> Result<Value, Error> {
    p.mutation.validate()?;
    value(service.create_version(&p.asset_id, &p.name, p.entry_id.as_ref(), &p.mutation.actor)?)
}

fn version_delete(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: VersionDelete,
) -> Result<Value, Error> {
    p.mutation.validate()?;
    value(service.delete_version(&p.asset_id, &p.name)?)
}

fn version_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: AssetParams,
) -> Result<Value, Error> {
    Ok(json!({"versions": service.versions(&p.asset_id)?}))
}

fn preset_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    _: NoParams,
) -> Result<Value, Error> {
    Ok(json!({"presets": service.presets()?}))
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
) -> Result<Value, Error> {
    p.mutation.validate()?;
    let preset =
        service.create_preset(&p.name, p.group.as_deref(), &p.settings, &p.mutation.actor)?;
    Ok(json!({"preset": preset}))
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
) -> Result<Value, Error> {
    p.mutation.validate()?;
    value(service.update_preset(
        &p.preset_id,
        &p.mutation.actor,
        p.name.as_deref(),
        p.group.as_deref(),
        p.settings.as_ref(),
    )?)
}

fn preset_delete(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: PresetDelete,
) -> Result<Value, Error> {
    p.mutation.validate()?;
    let outcome = service.delete_preset(&p.preset_id)?;
    Ok(json!({"outcome": outcome, "deleted": outcome == MutationOutcome::Applied}))
}

fn preset_export(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: PresetParams,
) -> Result<Value, Error> {
    value(service.export_preset(&p.preset_id)?)
}

fn preset_inspect(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: PresetInspect,
) -> Result<Value, Error> {
    service.inspect_import(&p.content, p.file_name.as_deref())
}

fn preset_import(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: PresetImport,
) -> Result<Value, Error> {
    p.mutation.validate()?;
    let preset = service.import_preset(
        &p.content,
        p.file_name.as_deref(),
        p.name.as_deref(),
        p.group.as_deref(),
        &p.mutation.actor,
    )?;
    Ok(json!({"report": preset.report, "preset": preset}))
}

fn preview_select(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: EntryParams,
) -> Result<Value, Error> {
    // The current entry is the live state, not a historical snapshot: selecting it is Return to
    // current, so the session keeps following later commits and editing stays enabled.
    let selection = if service.state(&p.asset_id)?.current_entry.id == p.entry_id {
        HistorySelection::Current
    } else {
        service.entry(&p.asset_id, &p.entry_id)?;
        HistorySelection::Entry(p.entry_id)
    };
    let generation = session.preview.select(selection);
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

fn recipe_describe(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: RecipeDescribe,
) -> Result<Value, Error> {
    value(service.describe_entry(&p.asset_id, p.entry_id.as_ref())?)
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
            return Err(Error::new(
                ErrorKind::Validation,
                format!("mode must be one of {}", modes.join(", ")),
            ));
        }
    }
    let mask_overlay = match &p.mask_overlay {
        None => None,
        Some(value) => Some(MaskOverlayMode::parse(value).ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                format!(
                    "mask_overlay must be one of {}",
                    MaskOverlayMode::ALL.map(MaskOverlayMode::as_str).join(", ")
                ),
            )
        })?),
    };
    let mask_overlay_colour = match &p.mask_overlay_colour {
        None => None,
        Some(value) => Some(MaskOverlayColour::parse(value).ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                format!(
                    "mask_overlay_colour must be one of {}",
                    MaskOverlayColour::ALL
                        .map(MaskOverlayColour::as_str)
                        .join(", ")
                ),
            )
        })?),
    };
    if let Some(Some(page)) = p.component_gallery
        && page >= COMPONENT_GALLERY_PAGE_COUNT
    {
        return Err(Error::new(
            ErrorKind::Validation,
            format!(
                "component_gallery must be null or an integer 0..{}",
                COMPONENT_GALLERY_PAGE_COUNT - 1
            ),
        ));
    }
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
    if let Some(page) = p.component_gallery {
        session.workspace.component_gallery = page;
    }
    session.touch();
    session_value(service, session)
}

fn session_state(
    service: &mut EditorService,
    session: &mut ClientSession,
    _: NoParams,
) -> Result<Value, Error> {
    session_value(service, session)
}

/// The process's resource counters and working-memory budgets. It takes no parameters and says so
/// when given one, so a client that expects an option here learns there is none.
fn resources_read(
    _: &mut EditorService,
    _: &mut ClientSession,
    _: NoParams,
) -> Result<Value, Error> {
    value(crate::resources::read())
}

fn render_sample(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: RenderSample,
) -> Result<Value, Error> {
    let mut sampled = match &p.draft_id {
        // A draft's effective recipe answers the point, so a readout during a gesture matches the
        // frame the same draft is previewing.
        Some(draft_id) => {
            let draft = session.held_draft(draft_id)?;
            value(service.sample_draft(&p.asset_id, draft, p.x, p.y)?)?
        }
        None => {
            let entry_id = selected_entry(service, session, &p.asset_id, None)?;
            value(service.sample_entry(&p.asset_id, &entry_id, p.x, p.y)?)?
        }
    };
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
/// A host command of the `mask.*` family resolves here as a module action does, so `draft.set`,
/// `draft.read` and `draft.reapply` validate a drafted gesture's fields against the same declared
/// parameters with the same generic check, and the conflict, Discard and Reapply rules below are the
/// delivered ones rather than a second copy.
fn draft_action<'a>(
    service: &'a EditorService,
    action_id: &str,
) -> Result<&'a ActionDescriptor, Error> {
    if let Some(command) = mask_commands::find(action_id) {
        return Ok(&command.action);
    }
    service
        .registry()
        .action(action_id)
        .map(|(_, action)| action)
        .ok_or_else(|| Error::new(ErrorKind::Validation, format!("unknown action {action_id}")))
}

fn draft_begin(
    service: &mut EditorService,
    session: &mut ClientSession,
    p: DraftBegin,
) -> Result<Value, Error> {
    if let Some(draft) = &session.draft {
        return Err(Error::new(
            ErrorKind::Conflict,
            format!(
                "this client already holds draft {} of action {}",
                draft.draft_id, draft.action
            ),
        ));
    }
    if !session.preview.can_edit() {
        return Err(Error::new(
            ErrorKind::Validation,
            "return to current before drafting an edit",
        ));
    }
    let _ = draft_action(service, &p.action)?;
    // A `mask.*` gesture says which mask and component it is editing, checked by the command itself.
    // A module action's draft takes the host's one optional `mask` field — the same field the
    // committed request carries — when its effect is maskable, so a masked slider previews what it
    // is about to commit instead of committing blind. It never takes a component: a module edits a
    // layer through the whole mask and knows nothing of the components that composed it. A rename is
    // not a gesture: it needs a `name`, which `draft.begin` does not take, so the command's own
    // envelope check refuses drafting it.
    let target = MaskTarget {
        mask: p.mask,
        component: p.component,
        name: None,
        // A stroke is deleted, never drafted: `mask.delete-stroke` requires one, so its own envelope
        // check refuses drafting it, exactly as a rename's missing `name` does.
        stroke: None,
    };
    let target = match mask_commands::find(&p.action) {
        Some(command) => {
            command.checked_target(&target)?;
            Some(target)
        }
        None if target == MaskTarget::default() => None,
        None if target.component.is_some() => {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("action {} takes no mask component", p.action),
            ));
        }
        None if service.registry().action_accepts_mask(&p.action) => Some(target),
        None => {
            return Err(Error::new(
                ErrorKind::Validation,
                format!("action {} does not accept a mask target", p.action),
            ));
        }
    };
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
    draft.checked_fields(&action.parameters, &p.fields)?;
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
) -> Result<Value, Error> {
    refresh_conflict(service, session)?;
    let draft = session.held_draft(&p.draft_id)?;
    if draft.conflicted {
        return Err(Error::new(
            ErrorKind::Conflict,
            "the asset changed under this draft; discard it or reapply it",
        ));
    }
    if p.mutation.expected_revision != draft.base_revision {
        return Err(Error::new(
            ErrorKind::Conflict,
            format!(
                "stale revision {}; this draft is based on revision {}",
                p.mutation.expected_revision, draft.base_revision
            ),
        ));
    }
    let asset_id = draft.asset_id.clone();
    let action = draft.action.clone();
    let target = draft.target.clone().unwrap_or_default();
    let mut fields = draft.fields.clone();
    // A failed commit keeps the draft, so the client can correct it and try again; a no-op ends it
    // exactly like an applied one, because the gesture is over either way. A drafted host command
    // commits through its own family; everything above this line — the conflict check, the revision
    // check and what a failure leaves behind — is the same for both.
    let result = match mask_commands::find(&action) {
        Some(command) => value(service.apply_mask_command(
            &asset_id,
            p.mutation,
            command,
            Value::Object(fields),
            target,
        )?)?,
        None => {
            // A module action's target is the host's one request field, so a drafted masked gesture
            // commits exactly the request an independent client would send for the same edit.
            if let Some(mask) = &target.mask {
                fields.insert(crate::MASK_FIELD.to_owned(), json!(mask));
            }
            value(service.apply_action(&asset_id, p.mutation, &action, Value::Object(fields))?)?
        }
    };
    session.draft = None;
    session.touch();
    Ok(result)
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
    draft.checked_fields(&action.parameters, &draft.fields)?;
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
    let entry_id = selected_entry(service, session, &p.asset_id, p.entry_id)?;
    value(service.locate_entry(&p.asset_id, &entry_id, p.x, p.y)?)
}

fn artifact_status(
    service: &mut EditorService,
    _: &mut ClientSession,
    _: NoParams,
) -> Result<Value, Error> {
    service.artifact_status()
}

fn artifact_inspect(
    service: &mut EditorService,
    _: &mut ClientSession,
    p: ArtifactInspect,
) -> Result<Value, Error> {
    service.inspect_artifact(&p.artifact_id)
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
    let entry_id = selected_entry(service, session, &p.asset_id, p.entry_id)?;
    value(service.transform_entry(&p.asset_id, &entry_id)?)
}

/// The entry a read-only question is answered against: the one the caller named, or the session's
/// selection. Every method that reads "the entry the client is looking at" resolves it here, so a
/// client previewing a historical entry asks about the stack it is looking at.
fn selected_entry(
    service: &EditorService,
    session: &ClientSession,
    asset_id: &AssetId,
    named: Option<EntryId>,
) -> Result<EntryId, Error> {
    match named {
        Some(entry_id) => Ok(entry_id),
        None => match &session.preview.selection {
            HistorySelection::Current => Ok(service.state(asset_id)?.current_entry.id),
            HistorySelection::Entry(id) => Ok(id.clone()),
        },
    }
}

fn require_current(session: &ClientSession) -> Result<(), Error> {
    if session.preview.can_edit() {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Conflict,
            "return to current or restore the selected history entry before editing",
        ))
    }
}

pub(super) fn value(value: impl Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiResponse;
    use crate::{
        ActionInput, ActionPlan, Availability, EFFECT_FORMAT, EffectDescriptor, EffectStage,
        ExactGeometry, ModuleDescriptor, ParameterDescriptor, ParameterKind, Processing, Stage,
        StageContext, ToolModule,
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
            None => Err(Error::new(
                ErrorKind::Protocol,
                format!("unknown method {method}"),
            )),
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

    #[test]
    fn host_and_generated_methods_are_unique_complete_and_match_the_schema() {
        let catalog =
            std::env::temp_dir().join(format!("lightwell-methods-{}.sqlite", std::process::id()));
        let mut service = EditorService::open(&catalog).unwrap();
        let mut session = ClientSession::default();
        let names: HashSet<&str> = METHODS.iter().map(|spec| spec.name).collect();
        assert_eq!(names.len(), METHODS.len(), "duplicate method names");
        let generated: Vec<String> = service
            .registry()
            .descriptors()
            .iter()
            .flat_map(|descriptor| descriptor.actions.iter())
            .map(|action| action_method(&action.id))
            .collect();
        assert_eq!(
            generated,
            [
                "edit.apply-preset",
                "edit.set-pixel",
                "edit.set-raw-exposure",
                "edit.set-raw-temperature",
                "edit.set-raw-tint",
                "edit.set-raw-red-gain",
                "edit.set-raw-blue-gain",
                "edit.pick-raw-neutral",
                "edit.use-as-shot-wb",
                "edit.reset-raw",
                "edit.set-basic",
                "edit.reset-basic",
                "edit.set-presence",
                "edit.reset-presence",
                "edit.set-mixer",
                "edit.reset-mixer",
                "edit.transform",
                "edit.crop",
                "edit.crop-fit",
                "edit.crop-reset",
                "edit.set-vignette",
                "edit.reset-vignette"
            ]
        );
        // A read-only module query generates a method of its own, in its own `query.` namespace.
        let queries: Vec<String> = service
            .registry()
            .descriptors()
            .iter()
            .flat_map(|descriptor| descriptor.queries.iter())
            .map(|query| query_method(&query.id))
            .collect();
        assert_eq!(queries, ["query.neutral-sample"]);
        // The host's own `mask.*` family, declared from the same descriptor types and listed from the
        // same table dispatch resolves through.
        let masks: Vec<&str> = mask_commands::all()
            .iter()
            .map(|command| command.method)
            .collect();
        assert_eq!(
            masks,
            [
                // The kind-independent commands, then three geometry methods per registered
                // component kind, generated from the host's own kind table.
                "mask.list",
                "mask.sample-input",
                "mask.delete",
                "mask.rename",
                "mask.duplicate",
                "mask.set-amount",
                "mask.set-invert",
                "mask.reorder",
                "mask.set-component-mode",
                "mask.set-component-invert",
                "mask.delete-component",
                "mask.reorder-component",
                "mask.add-stroke",
                "mask.delete-stroke",
                "mask.create-linear",
                "mask.add-linear",
                "mask.set-linear",
                "mask.create-radial",
                "mask.add-radial",
                "mask.set-radial",
                "mask.create-luminance-range",
                "mask.add-luminance-range",
                "mask.set-luminance-range",
                "mask.create-colour-range",
                "mask.add-colour-range",
                "mask.set-colour-range",
                // And, for the one kind that holds a list of picked colours, the two sample methods
                // the same table generates.
                "mask.add-colour-range-sample",
                "mask.delete-colour-range-sample"
            ]
        );
        let schema = schemas(service.registry());
        let listed = schema["methods"].as_object().unwrap();
        assert_eq!(
            listed.len(),
            METHODS.len() + generated.len() + queries.len() + masks.len()
        );
        for method in &masks {
            assert!(listed.contains_key(*method), "{method} is not discoverable");
            assert!(
                matches!(find(&service, method), Some(Method::Mask(_))),
                "{method} does not dispatch as a host mask command"
            );
        }
        assert_eq!(
            listed["mask.list"]["mutates"],
            json!(false),
            "mask.list writes nothing"
        );
        assert_eq!(
            listed["query.neutral-sample"]["mutates"],
            json!(false),
            "a query writes nothing"
        );
        assert_eq!(
            listed["query.neutral-sample"]["required"],
            json!(["asset_id", "x", "y"])
        );
        assert_eq!(
            listed["query.neutral-sample"]["optional"]
                .as_object()
                .expect("the query's optional fields")
                .keys()
                .collect::<Vec<_>>(),
            ["entry_id"],
            "a query answers about the session's selection unless an entry is named"
        );
        assert_eq!(
            schema["modules"].as_array().unwrap().len(),
            service.registry().descriptors().len()
        );
        assert_eq!(
            listed["edit.set-pixel"]["required"],
            json!(["asset_id", "mutation", "x", "y", "rgb"])
        );
        assert_eq!(
            listed["edit.set-pixel"]["parameters"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert_eq!(listed["edit.transform"]["mutates"], json!(true));
        // Each host method's schema is generated from the struct its handler parses, so the lists
        // are the parser's own; the owner's generated test sends every method its declared fields.
        for spec in METHODS {
            let schema = &listed[spec.name];
            assert_eq!(
                schema["required"],
                json!(spec.params.required),
                "{}",
                spec.name
            );
            assert_eq!(
                schema["optional"]
                    .as_object()
                    .expect("the optional fields")
                    .len(),
                spec.params.optional.len(),
                "{}",
                spec.name
            );
            // A method mutates exactly when it carries a mutation envelope, and says which.
            assert_eq!(schema["mutates"], json!(spec.mutates()), "{}", spec.name);
            assert_eq!(
                schema.get("mutation").cloned(),
                spec.params.envelope.name().map(|name| json!(name)),
                "{}",
                spec.name
            );
            assert_eq!(
                spec.params.required.contains(&"mutation"),
                spec.mutates(),
                "{} carries its envelope as the required mutation field",
                spec.name
            );
        }
        // Every generated action commits a revisioned change to an asset.
        assert_eq!(listed["edit.set-pixel"]["mutation"], json!("revision"));
        assert_eq!(listed["mask.delete"]["mutation"], json!("revision"));
        assert_eq!(listed["mask.list"].get("mutation"), None);
        for (name, envelope) in [
            ("history.undo", "revision"),
            ("draft.commit", "revision"),
            ("module.settings.set", "revision"),
            ("preset.create", "request"),
            ("version.create", "request"),
            ("catalog.import", "request"),
            ("artifact.collect", "request"),
            ("module.permission.grant", "request"),
            ("module.activate", "request"),
            ("module.resource.install", "request"),
            ("module.job.cancel", "request"),
        ] {
            assert_eq!(listed[name]["mutation"], json!(envelope), "{name}");
        }
        assert_eq!(
            schema["mutation"]["revision"],
            json!(["expected_revision", "request_id", "actor"])
        );
        assert_eq!(
            schema["mutation"]["request"],
            json!(["request_id", "actor"])
        );
        // The descriptor additions the workspace renders from reach a client through module.list.
        let modules = ok(&mut service, &mut session, "module.list", json!({}));
        let module = |id: &str| -> Value {
            modules["modules"]
                .as_array()
                .unwrap()
                .iter()
                .find(|module| module["id"] == json!(id))
                .unwrap_or_else(|| panic!("{id} is registered"))
                .clone()
        };
        let pixel = module("lightwell.pixel");
        assert_eq!(pixel["hint"], json!("One exact pixel"));
        assert_eq!(pixel["developer"], json!(true));
        assert_eq!(pixel["reset"], json!(null));
        assert_eq!(pixel["canvas"]["title"], json!("Pick pixel"));
        assert_eq!(pixel["canvas"]["shortcut"], json!(null));
        // Every pick mode is discovered as a control of its own module, so a client reaches it
        // from that module's panel and not only from a mode strip it has to invent.
        let picker = |module: &Value| -> Value {
            module["controls"][0]["controls"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .find(|control| control["kind"] == json!("picker"))
                .cloned()
                .unwrap_or(Value::Null)
        };
        assert_eq!(
            picker(&pixel),
            json!({"kind": "picker", "label": "Pick pixel"})
        );
        assert_eq!(
            picker(&module("lightwell.basic")),
            json!({"kind": "picker", "label": "Neutral picker"}),
            "the neutral picker is a control of the White balance group"
        );
        assert_eq!(
            picker(&module("lightwell.raw")),
            json!({"kind": "picker", "label": "Neutral WB"})
        );
        assert_eq!(pixel["actions"][0]["summary"], json!("Pixel {x}, {y}"));
        let transform = module("lightwell.transform");
        assert_eq!(transform["hint"], json!("Rotate, mirror and flip"));
        assert_eq!(transform["developer"], json!(false));
        assert_eq!(transform["actions"][0]["summary"], json!("{transform}"));
        let crop = module("lightwell.crop");
        assert_eq!(crop["hint"], json!("Frame, ratio and angle"));
        assert_eq!(crop["reset"], json!({"action": "crop-reset", "preset": {}}));
        assert_eq!(
            crop["controls"],
            json!([]),
            "the crop reset moved from a control to the section header"
        );
        assert_eq!(crop["canvas"]["title"], json!("Crop"));
        assert_eq!(crop["canvas"]["shortcut"], json!("R"));
        // A preset is applied through one generated method, whose parameters carry their kinds.
        let apply = &listed["edit.apply-preset"];
        assert_eq!(apply["mutates"], json!(true));
        assert_eq!(apply["patch"], json!(false));
        assert_eq!(
            apply["required"],
            json!(["asset_id", "mutation", "settings", "name"])
        );
        assert_eq!(
            apply["optional"]
                .as_object()
                .expect("the optional fields")
                .keys()
                .collect::<Vec<_>>(),
            ["preset-id"]
        );
        assert_eq!(
            apply["parameters"]
                .as_array()
                .expect("the declared parameters")
                .iter()
                .map(|parameter| json!([
                    parameter["name"],
                    parameter["kind"],
                    parameter.get("max_length").cloned().unwrap_or(Value::Null),
                    parameter["required"],
                ]))
                .collect::<Vec<_>>(),
            [
                json!(["settings", "settings", null, true]),
                json!(["name", "string", 128, true]),
                json!(["preset-id", "string", 96, false]),
            ]
        );
        // The presets module leads module.list with its one presets control and no effect.
        let presets = &modules["modules"][0];
        assert_eq!(presets["id"], json!("lightwell.presets"));
        assert_eq!(presets["title"], json!("Presets"));
        assert_eq!(presets["hint"], json!("Saved and imported settings"));
        assert_eq!(presets["collapsed"], json!(true));
        assert_eq!(presets["effects"], json!([]));
        assert_eq!(
            presets["controls"],
            json!([{"kind": "presets", "action": "apply-preset"}])
        );
        assert_eq!(presets, &module("lightwell.presets"));
        // Every module lists its layout hint, stacked by default; the mixer declares tabs because
        // its three groups are parallel views of the same eight ranges.
        assert_eq!(module("lightwell.mixer")["layout"], json!("tabs"));
        assert_eq!(module("lightwell.basic")["layout"], json!("stacked"));
        for name in names
            .iter()
            .copied()
            .chain(generated.iter().map(String::as_str))
        {
            let spec = find(&service, name).expect("listed methods resolve");
            if matches!(spec.route(), Route::Service) {
                let response = call(&mut service, &mut session, name, json!({}));
                if let Some(error) = &response.error {
                    assert_ne!(error.code, "internal", "{name}: {}", error.message);
                }
            }
            assert!(
                !mutates(&spec, Some(&json!({"outcome": "no-op"}))),
                "{name} must not emit events for a no-op"
            );
            assert!(
                !mutates(
                    &spec,
                    Some(&json!({"outcome": "applied", "deduplicated": true}))
                ),
                "{name} must not emit a second event for a retry"
            );
            assert_eq!(
                mutates(&spec, Some(&json!({"outcome": "applied"}))),
                spec.mutates()
            );
            assert!(!listed[name]["notes"].as_str().unwrap().is_empty());
        }
        assert!(find(&service, "edit.missing").is_none());
        assert!(find(&service, "set-pixel").is_none());
        assert!(
            schema["coordinate_space"]
                .as_str()
                .expect("the coordinate note")
                .contains("number parameter"),
            "the schema describes number parameters"
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
            "lightwell-methods-resources-{}.sqlite",
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
            assert!(
                !mutates(&method, Some(&read)),
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
            "lightwell-methods-tasks-{}.sqlite",
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
                + mask_commands::all().len()
                + count(|descriptor| descriptor.actions.len())
                + count(|descriptor| descriptor.queries.len())
                + tasks.len()
        );
        for name in listed.keys() {
            let method = find(&service, name).expect("every listed method resolves");
            assert_eq!(
                name.starts_with(TASK_PREFIX),
                matches!(method, Method::Task),
                "{name}"
            );
        }
        let method = find(&service, &tasks[0]).unwrap();
        assert!(matches!(method.route(), Route::Owner(_)));
        assert!(
            !method.mutates(),
            "the request queues a job and writes nothing"
        );
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
            std::env::temp_dir().join(format!("lightwell-locate-{}.sqlite", std::process::id()));
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
            std::env::temp_dir().join(format!("lightwell-transform-{}.sqlite", std::process::id()));
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
        fn describe_layer(&self, _: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok("Angle".into())
        }
        fn compile(&self, _: &str, _: u32, _: &Value, _: Stage) -> Result<Processing, Error> {
            Err(Error::new(ErrorKind::Internal, "test module never renders"))
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
                }],
                actions: vec![ActionDescriptor {
                    id: "test-angle".into(),
                    title: "Set angle".into(),
                    notes: "test".into(),
                    summary: Some("Angle {angle}".into()),
                    patch: false,
                    parameters: vec![ParameterDescriptor {
                        name: "angle".into(),
                        kind: ParameterKind::Number {
                            min: -45.0,
                            max: 45.0,
                        },
                        required: true,
                        default: None,
                        unit: Some("deg".into()),
                        step: None,
                        precision: None,
                        notes: "test".into(),
                        soft_min: None,
                        soft_max: None,
                        fine_step: None,
                        zero: None,
                    }],
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
            "lightwell-methods-number-{}.sqlite",
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
                }],
                actions: vec![ActionDescriptor {
                    id: MARK_ACTION.into(),
                    title: "Mark".into(),
                    notes: "test".into(),
                    summary: None,
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
        fn describe_layer(&self, _: &str, _: u32, _: &Value) -> Result<String, Error> {
            Ok("Marked".into())
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

    #[test]
    fn recipe_describe_lists_every_layer_and_names_a_provider_it_cannot_use() {
        let catalog = std::env::temp_dir().join(format!(
            "lightwell-methods-describe-{}.sqlite",
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
                        json!("lightwell.transform"),
                        json!("Rotate left"),
                        json!(true)
                    ),
                    (json!("test.mark"), json!("Marked"), json!(true)),
                ]
            );
            assert_eq!(described["layers"][1]["effect"], json!(MARK_EFFECT));
            assert_eq!(described["layers"][1]["title"], json!("Mark"));
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
            "lightwell-methods-workspace-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut service = EditorService::open(&catalog).unwrap();
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
                "component_gallery": null,
            }),
            "a fresh session opens with both panels, the pointer and no overlay"
        );
        let set = ok(
            &mut service,
            &mut session,
            "workspace.set",
            json!({"state_panel": false, "mode": "lightwell.crop", "thirds": true}),
        );
        assert_eq!(
            set["workspace"],
            json!({
                "state_panel": false,
                "tools_panel": true,
                "mode": "lightwell.crop",
                "thirds": true,
                "clip_shadows": false,
                "clip_highlights": false,
                "mask_overlay": "off",
                "mask_overlay_colour": "green",
                "component_gallery": null,
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
                json!({"mode": "lightwell.heal"}),
                // The host modes first — the pointer, the mask mode and one per canvas pick the host
                // declares for a sampling component kind — then the registry's canvas declarations,
                // so the RAW and Basic neutral pickers join the list without a change here.
                "mode must be one of pointer, mask, mask.add-colour-range-sample, lightwell.pixel, \
                 lightwell.raw, lightwell.basic, lightwell.crop",
            ),
            (
                "a module that declares no canvas",
                json!({"mode": "lightwell.transform"}),
                "mode must be one of",
            ),
            ("an unknown field", json!({"panel": true}), "unknown field"),
            ("the wrong type", json!({"thirds": "yes"}), "invalid type"),
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
            "lightwell-methods-paths-{}.sqlite",
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
        assert_eq!(
            paths["decimation_tolerance"],
            json!(path::DECIMATION_TOLERANCE)
        );
        assert_eq!(paths["stored_deviation"], json!(path::STORED_DEVIATION));
        assert_eq!(paths["points_per_stroke"], json!(1024));
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
        let declared = crate::ParameterDescriptor {
            name: "path".into(),
            kind: crate::ParameterKind::Points {
                points_min: 1,
                points_max: 512,
            },
            required: true,
            default: None,
            unit: None,
            step: None,
            precision: None,
            soft_min: None,
            soft_max: None,
            fine_step: None,
            zero: None,
            notes: "the drawn path".into(),
        };
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
    fn component_gallery_page_is_a_validated_per_client_workspace_preference() {
        let catalog = std::env::temp_dir().join(format!(
            "lightwell-methods-gallery-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut service = EditorService::open(&catalog).unwrap();
        let mut session = ClientSession::default();

        let listed = ok(&mut service, &mut session, "schema.list", json!({}));
        let gallery = &listed["methods"]["workspace.set"];
        assert_eq!(gallery["mutates"], json!(false));
        assert!(
            gallery["optional"]["component_gallery"]
                .as_str()
                .unwrap()
                .contains("integer 0..9")
        );
        assert!(
            gallery["notes"]
                .as_str()
                .unwrap()
                .contains("needs no asset")
        );

        let opened = ok(
            &mut service,
            &mut session,
            "workspace.set",
            json!({"component_gallery": 0, "tools_panel": false}),
        );
        assert_eq!(opened["workspace"]["component_gallery"], json!(0));
        assert_eq!(opened["workspace"]["tools_panel"], json!(false));
        assert_eq!(opened["workspace"]["state_panel"], json!(true));
        assert_eq!(session.preview, crate::PreviewSession::default());

        let switched = ok(
            &mut service,
            &mut session,
            "workspace.set",
            json!({"component_gallery": COMPONENT_GALLERY_PAGE_COUNT - 1}),
        );
        assert_eq!(switched["workspace"]["component_gallery"], json!(9));
        assert_eq!(switched["workspace"]["tools_panel"], json!(false));
        let preserved = session.clone();
        for (case, params) in [
            (
                "above last page",
                json!({"component_gallery": 10, "state_panel": false}),
            ),
            (
                "negative page",
                json!({"component_gallery": -1, "state_panel": false}),
            ),
            (
                "fractional page",
                json!({"component_gallery": 1.5, "state_panel": false}),
            ),
            (
                "string page",
                json!({"component_gallery": "0", "state_panel": false}),
            ),
        ] {
            let rejected = call(&mut service, &mut session, "workspace.set", params);
            assert_eq!(rejected.error.unwrap().code, "validation", "{case}");
            assert_eq!(
                session, preserved,
                "{case} must change no workspace field or revision"
            );
        }
        let unchanged = ok(&mut service, &mut session, "workspace.set", json!({}));
        assert_eq!(unchanged["workspace"]["component_gallery"], json!(9));
        let closed = ok(
            &mut service,
            &mut session,
            "workspace.set",
            json!({"component_gallery": null}),
        );
        assert_eq!(closed["workspace"]["component_gallery"], json!(null));
        assert_eq!(closed["workspace"]["tools_panel"], json!(false));
        assert_eq!(closed["workspace"]["state_panel"], json!(true));
        assert_eq!(session.preview, crate::PreviewSession::default());
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }

    /// A service serving the built-ins plus the test patch module, with one asset imported. The
    /// patch module is the shape a field-patch tool takes: one layer, merged field by field.
    fn patched(name: &str) -> (EditorService, PathBuf, Value) {
        let catalog = std::env::temp_dir().join(format!(
            "lightwell-methods-{name}-{}.sqlite",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&catalog);
        let mut registry = ModuleRegistry::builtin();
        registry.register(PatchModule::shared()).unwrap();
        let mut service = EditorService::open_with(&catalog, Arc::new(registry)).unwrap();
        let asset = json!(service.import(&fixture()).unwrap().asset.id);
        (service, catalog, asset)
    }

    fn mutation(revision: u64, request: &str) -> Value {
        json!({"expected_revision": revision, "request_id": request, "actor": "test"})
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
        let spec = find(&service, "edit.apply-preset").expect("a generated method");
        let settings = json!({"set-patch": {"red": 12.0}, "set-basic": {"exposure": 0.5}});
        let applied = ok(
            &mut service,
            &mut session,
            "edit.apply-preset",
            json!({
                "asset_id": asset,
                "mutation": mutation(0, "preset"),
                "settings": settings,
                "name": "Warm",
                "preset-id": "preset-7",
            }),
        );
        assert_eq!(applied["outcome"], json!("applied"));
        assert_eq!(applied["revision"], json!(1));
        assert!(mutates(&spec, Some(&applied)));
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
                (json!("lightwell.basic"), Value::Null),
                (json!(PATCH_MODULE), json!(12.0)),
            ],
            "steps run in key order, so the patch layer joins the content region after Basic's, \
             exactly as sending the two actions in that order does"
        );

        let again = ok(
            &mut service,
            &mut session,
            "edit.apply-preset",
            json!({
                "asset_id": asset,
                "mutation": mutation(1, "again"),
                "settings": settings,
                "name": "Warm",
            }),
        );
        assert_eq!(again["outcome"], json!("no-op"));
        assert_eq!(again["created_entry_id"], json!(null));
        assert!(!mutates(&spec, Some(&again)));

        let refused = call(
            &mut service,
            &mut session,
            "edit.apply-preset",
            json!({
                "asset_id": asset,
                "mutation": mutation(1, "refused"),
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
            params["mutation"] = mutation(revision, request);
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

        // A patch the module has nothing to say about falls back to the summary template.
        let both = patch(
            &mut service,
            &mut session,
            2,
            "both",
            json!({"red": 1.0, "green": 2.0}),
        );
        assert_eq!(
            entry_of(&mut service, &mut session, &asset, &both)["label"],
            json!("Patch 1 2")
        );

        // The crop module reports its frame the same way, so a client seeds its controls from the
        // displayed entry instead of parsing payloads itself.
        ok(
            &mut service,
            &mut session,
            "edit.crop",
            json!({"asset_id": asset, "mutation": mutation(3, "crop"), "x": 0.0, "y": 0.0, "width": 0.5, "height": 0.5}),
        );
        let rows = described(&mut service, &mut session, &asset);
        let crop = rows["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|layer| layer["module"] == json!("lightwell.crop"))
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
            json!({"draft_id": draft_id, "mutation": mutation(0, "gesture")}),
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
            json!({"draft_id": draft_id, "mutation": mutation(0, "single")}),
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
            json!({"asset_id": asset, "mutation": mutation(0, "one"), "red": 5.0}),
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
            json!({"asset_id": asset, "mutation": mutation(0, "start"), "red": 10.0}),
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
            json!({"draft_id": draft_id, "mutation": mutation(1, "return")}),
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
            json!({"asset_id": asset, "mutation": mutation(0, "agent"), "green": 60.0}),
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
            json!({"draft_id": draft_id, "mutation": mutation(0, "editor")}),
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
            json!({"draft_id": draft_id, "mutation": mutation(1, "editor")}),
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
            json!({"draft_id": draft_id, "mutation": mutation(1, "stale")}),
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
            json!({"draft_id": draft_id, "mutation": mutation(0, "gesture")}),
        );
        assert_eq!(committed["outcome"], json!("applied"));

        // The commit is an ordinary action underneath, so retrying the identical request returns
        // the original result and writes no second entry.
        let retried = ok(
            &mut service,
            &mut session,
            &patch_method(),
            json!({"asset_id": asset, "mutation": mutation(0, "gesture"), "red": 10.0}),
        );
        assert_eq!(retried["deduplicated"], json!(true));
        assert_eq!(retried["current_entry_id"], committed["current_entry_id"]);
        assert_eq!(retried["revision"], committed["revision"]);
        let reused = call(
            &mut service,
            &mut session,
            &patch_method(),
            json!({"asset_id": asset, "mutation": mutation(0, "gesture"), "red": 11.0}),
        )
        .error
        .expect("the same request id with different input is a conflict");
        assert_eq!(reused.code, "conflict");
        drop(service);
        std::fs::remove_file(catalog).unwrap();
    }
}
