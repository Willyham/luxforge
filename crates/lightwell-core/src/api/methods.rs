//! The method table: host methods carry their schema description, mutation flag and handler, and
//! every module action resolves to a generated `edit.<action>` method from the same registry, so
//! discovery, event emission and dispatch cannot drift apart.
use super::{
    ApiRequest, ApiResponse, COMPONENT_GALLERY_PAGE_COUNT, ClientSession, POINTER_MODE, PROTOCOL,
};
use crate::{
    ActionDescriptor, AssetId, Draft, DraftId, EditorService, EntryId, Error, ErrorKind,
    HistorySelection, ModuleRegistry, Mutation, MutationOutcome, PresetId, Zoom,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};

pub(super) type Handler =
    fn(&mut EditorService, &mut ClientSession, &Value) -> Result<Value, Error>;

pub(super) struct MethodSpec {
    pub name: &'static str,
    pub mutates: bool,
    pub required: &'static [&'static str],
    pub optional: &'static [(&'static str, &'static str)],
    pub notes: &'static str,
    /// `None` marks a method the owner loop answers from its own state.
    pub handler: Option<Handler>,
}

pub(super) const METHODS: &[MethodSpec] = &[
    MethodSpec {
        name: "schema.list",
        mutates: false,
        required: &[],
        optional: &[],
        notes: "protocol identity and every method with its parameters",
        handler: Some(schema_list),
    },
    MethodSpec {
        name: "catalog.import",
        mutates: true,
        required: &["path"],
        optional: &[],
        notes: "queues bounded source preparation; returns a job to inspect with job.status; commits only on verified success",
        handler: None,
    },
    MethodSpec {
        name: "job.status",
        mutates: false,
        required: &["job_id"],
        optional: &[],
        notes: "this client's bounded source job state; ready includes the committed asset state",
        handler: None,
    },
    MethodSpec {
        name: "job.adopt",
        mutates: false,
        required: &["job_id"],
        optional: &[],
        notes: "select the ready result of this client's latest import as current; stale imports are refused",
        handler: None,
    },
    MethodSpec {
        name: "job.cancel",
        mutates: false,
        required: &["job_id"],
        optional: &[],
        notes: "remove this client's interest in a source job without cancelling other clients",
        handler: None,
    },
    MethodSpec {
        name: "source.prepare",
        mutates: false,
        required: &["asset_id"],
        optional: &[("entry_id", "historical entry; default current")],
        notes: "queue signature-verified preparation of an imported source after reopen or cache eviction",
        handler: None,
    },
    MethodSpec {
        name: "catalog.list",
        mutates: false,
        required: &[],
        optional: &[],
        notes: "referenced assets in import order",
        handler: Some(catalog_list),
    },
    MethodSpec {
        name: "asset.state",
        mutates: false,
        required: &["asset_id"],
        optional: &[],
        notes: "current entry, revision and redo path",
        handler: Some(asset_state),
    },
    MethodSpec {
        name: "source.inspect",
        mutates: false,
        required: &["asset_id"],
        optional: &[("entry_id", "historical entry; default current")],
        notes: "persisted source identity, RAW interpretation, crop, backend and preparation readiness without decoding",
        handler: Some(source_inspect),
    },
    MethodSpec {
        name: "history.list",
        mutates: false,
        required: &["asset_id"],
        optional: &[("before_sequence", "u64"), ("limit", "1..100")],
        notes: "chronological entries newest first, including abandoned branches",
        handler: Some(history_list),
    },
    MethodSpec {
        name: "history.inspect",
        mutates: false,
        required: &["asset_id", "entry_id"],
        optional: &[],
        notes: "one entry with its complete immutable stack",
        handler: Some(history_inspect),
    },
    MethodSpec {
        name: "history.lineage",
        mutates: false,
        required: &["asset_id"],
        optional: &[
            ("entry_id", "start entry; default current"),
            ("limit", "1..100"),
        ],
        notes: "undo-parent chain newest first; next_entry_id continues a longer chain",
        handler: Some(history_lineage),
    },
    MethodSpec {
        name: "recipe.describe",
        mutates: false,
        required: &["asset_id"],
        optional: &[("entry_id", "entry to describe; default current")],
        notes: "an entry's stored layers in order with their module, title, summary and availability; reads payloads only and renders nothing",
        handler: Some(recipe_describe),
    },
    MethodSpec {
        name: "module.list",
        mutates: false,
        required: &[],
        optional: &[("asset_id", "filter controls for this asset source kind")],
        notes: "every registered module descriptor with its effects, actions, parameters and controls",
        handler: Some(module_list),
    },
    MethodSpec {
        name: "history.undo",
        mutates: true,
        required: &["asset_id", "mutation"],
        optional: &[],
        notes: "moves current to its undo parent without adding an entry",
        handler: Some(history_undo),
    },
    MethodSpec {
        name: "history.redo",
        mutates: true,
        required: &["asset_id", "mutation"],
        optional: &[],
        notes: "follows the persisted redo path",
        handler: Some(history_redo),
    },
    MethodSpec {
        name: "history.restore",
        mutates: true,
        required: &["asset_id", "mutation", "entry_id"],
        optional: &[],
        notes: "appends a restore action copying the entry's stack and returns the session to current",
        handler: Some(history_restore),
    },
    MethodSpec {
        name: "version.create",
        mutates: true,
        required: &["asset_id", "name", "actor"],
        optional: &[("entry_id", "entry to name; default current")],
        notes: "names a retained entry; unique per asset ignoring case; no-op when the name already names that entry",
        handler: Some(version_create),
    },
    MethodSpec {
        name: "version.delete",
        mutates: true,
        required: &["asset_id", "name"],
        optional: &[],
        notes: "removes the name only; the entry stays in history; no-op when absent",
        handler: Some(version_delete),
    },
    MethodSpec {
        name: "version.list",
        mutates: false,
        required: &["asset_id"],
        optional: &[],
        notes: "saved versions in creation order with their entry sequence",
        handler: Some(version_list),
    },
    // The preset library is catalog data beside history. None of these methods renders, opens a
    // source or hashes pixels; applying a preset is edit.apply-preset.
    MethodSpec {
        name: "preset.list",
        mutates: false,
        required: &[],
        optional: &[],
        notes: "{presets: [{id, name, group, settings, origin, report, actor, created_ms, updated_ms, unavailable}]} sorted by group, then name, ignoring case; report is the import report's counts {mapped, neutral, unsupported, refused}, or null for a preset created in Lightwell; unavailable names the settings actions this registry cannot apply; no source_text",
        handler: Some(preset_list),
    },
    MethodSpec {
        name: "preset.read",
        mutates: false,
        required: &["preset_id"],
        optional: &[],
        notes: "{preset}: one record with its full import report and source_text, the imported file's text kept verbatim, or null for a preset created in Lightwell",
        handler: Some(preset_read),
    },
    MethodSpec {
        name: "preset.create",
        mutates: true,
        required: &["name", "settings", "actor"],
        optional: &[("group", "1..64 printable characters; default User presets")],
        notes: "{preset}: stores a settings set as a Lightwell preset with origin {kind: lightwell} and report null; name is 1..128 and group 1..64 printable characters after trimming, the (group, name) pair is unique ignoring case (a duplicate is a conflict) and the library holds at most 1000 presets (resource-limit); every action and field is checked against the registry",
        handler: Some(preset_create),
    },
    MethodSpec {
        name: "preset.capture",
        mutates: false,
        required: &["asset_id", "fields"],
        optional: &[("entry_id", "entry to read; default the session's selection")],
        notes: "{settings} read from one entry's stack: fields maps field-patch actions to an array of their parameter names or true for all of them; each field takes the value of its module's one layer, or its declared default when the stack has none; two or more layers are validation: ambiguous; reads stored payloads only, so it opens no source and renders nothing; send the result to preset.create",
        handler: Some(preset_capture),
    },
    MethodSpec {
        name: "preset.update",
        mutates: true,
        required: &["preset_id", "actor"],
        optional: &[
            ("name", "1..128 printable characters"),
            ("group", "1..64 printable characters"),
            ("settings", "a settings set checked against the registry"),
        ],
        notes: "{outcome, preset}: applied when the name, group or settings change, recording the actor and updated_ms; no-op, with nothing written, when they do not; a rename onto another preset's (group, name) pair is a conflict; origin and report are kept",
        handler: Some(preset_update),
    },
    MethodSpec {
        name: "preset.delete",
        mutates: true,
        required: &["preset_id"],
        optional: &[],
        notes: "{outcome, deleted}: applied and true when the preset existed, no-op and false when it is absent; history entries that applied it are unchanged",
        handler: Some(preset_delete),
    },
    MethodSpec {
        name: "preset.export",
        mutates: false,
        required: &["preset_id"],
        optional: &[],
        notes: "{file_name, content}: the preset as a Lightwell preset document named <name>.lwpreset, which preset.import reads back to the same name, group and settings",
        handler: Some(preset_export),
    },
    MethodSpec {
        name: "preset.inspect",
        mutates: false,
        required: &["content"],
        optional: &[(
            "file_name",
            "the file's name, for the fallback preset name and the origin",
        )],
        notes: "dry run of preset.import that stores nothing: {preset, report}, where preset has the record's shape with id, actor, created_ms and updated_ms null, the file's name and the file's group or Imported, and report is the full per-setting import report; a file that maps nothing still returns its report with empty settings",
        handler: Some(preset_inspect),
    },
    MethodSpec {
        name: "preset.import",
        mutates: true,
        required: &["content", "actor"],
        optional: &[
            (
                "file_name",
                "the file's name, for the fallback preset name and the origin",
            ),
            ("name", "overrides the file's name"),
            ("group", "overrides the file's group; default Imported"),
        ],
        notes: "{preset, report}: reads the text of a Lightwell preset document, a Lightroom XMP preset or a .lrtemplate, at most 1 MiB, and stores its mapped settings with the text kept verbatim; the library's name, uniqueness and size rules apply as for preset.create; a file that maps nothing is unsupported-input with the report counts, and a refused import stores nothing",
        handler: Some(preset_import),
    },
    MethodSpec {
        name: "preview.select",
        mutates: false,
        required: &["asset_id", "entry_id"],
        optional: &[],
        notes: "read-only session selection; the current entry selects current, not a historical preview; returns generation and session",
        handler: Some(preview_select),
    },
    MethodSpec {
        name: "preview.return-current",
        mutates: false,
        required: &[],
        optional: &[],
        notes: "returns generation and session",
        handler: Some(preview_return_current),
    },
    MethodSpec {
        name: "view.set",
        mutates: false,
        required: &[],
        optional: &[
            ("zoom", "{mode:fit} or {mode:percent,value:10..1600}"),
            ("pan_x", "finite"),
            ("pan_y", "finite"),
        ],
        notes: "session view state; returns the session",
        handler: Some(view_set),
    },
    MethodSpec {
        name: "workspace.set",
        mutates: false,
        required: &[],
        optional: &[
            ("state_panel", "bool"),
            ("tools_panel", "bool"),
            (
                "mode",
                "pointer or an available module id that declares a canvas interaction",
            ),
            ("thirds", "bool"),
            ("clip_shadows", "bool; show the shadow clipping overlay"),
            (
                "clip_highlights",
                "bool; show the highlight clipping overlay",
            ),
            (
                "component_gallery",
                "null closes the diagnostic components board; integer 0..9 selects a page",
            ),
        ],
        notes: "per-client screen preference: panels, canvas mode, overlays and diagnostic components page; needs no asset and changes no history or frame; returns the session",
        handler: Some(workspace_set),
    },
    MethodSpec {
        name: "session.state",
        mutates: false,
        required: &[],
        optional: &[],
        notes: "this client's selection, view, workspace state and session revision",
        handler: Some(session_state),
    },
    MethodSpec {
        name: "draft.begin",
        mutates: false,
        required: &["asset_id", "action"],
        optional: &[],
        notes: "opens this client's one draft of that action, bound to the asset's current revision; refused while a draft is open or a historical entry is previewed",
        handler: Some(draft_begin),
    },
    MethodSpec {
        name: "draft.set",
        mutates: false,
        required: &["draft_id", "fields"],
        optional: &[],
        notes: "validates the named fields against the action's parameters and merges them into the draft; an invalid field changes nothing",
        handler: Some(draft_set),
    },
    MethodSpec {
        name: "draft.read",
        mutates: false,
        required: &["draft_id"],
        optional: &[],
        notes: "the draft with conflicted recomputed against the asset's current revision",
        handler: Some(draft_read),
    },
    MethodSpec {
        name: "draft.cancel",
        mutates: false,
        required: &["draft_id"],
        optional: &[],
        notes: "ends the draft and commits nothing",
        handler: Some(draft_cancel),
    },
    MethodSpec {
        name: "draft.commit",
        mutates: true,
        required: &["draft_id", "mutation"],
        optional: &[],
        notes: "runs the draft's action with its accumulated fields and ends the draft; a conflicted draft or a mismatched expected_revision is refused and the draft is kept",
        handler: Some(draft_commit),
    },
    MethodSpec {
        name: "draft.reapply",
        mutates: false,
        required: &["draft_id"],
        optional: &[],
        notes: "rebases the draft on the asset's current revision, keeping and revalidating only the fields this client set",
        handler: Some(draft_reapply),
    },
    MethodSpec {
        name: "render.sample",
        mutates: false,
        required: &["asset_id", "x", "y"],
        optional: &[(
            "draft_id",
            "this client's draft to sample instead of the stored stack",
        )],
        notes: "one pixel of the session's selected entry, or of an open draft's effective recipe, evaluated without rasterizing",
        handler: Some(render_sample),
    },
    MethodSpec {
        name: "render.locate",
        mutates: false,
        required: &["asset_id", "x", "y"],
        optional: &[(
            "entry_id",
            "entry to locate in; default the session's selection",
        )],
        notes: "the content pixel, the source after EXIF orientation, that one output pixel shows",
        handler: Some(render_locate),
    },
    MethodSpec {
        name: "events.since",
        mutates: false,
        required: &["after"],
        optional: &[],
        notes: "gap=true requires an asset.state refresh",
        handler: None,
    },
    // The analysis methods are answered by the catalog owner, because the job store, the worker
    // slots and every client's draft live there. They mutate nothing and emit no event.
    MethodSpec {
        name: "analysis.request",
        mutates: false,
        required: &["asset_id", "target"],
        optional: &[],
        notes: "queues the exact RGB histogram and output-clipping reduction of one evaluated stack and returns {job_id, status, identity} promptly, with the report included when the store already holds it; target is {kind:current}, {kind:entry,entry_id} or {kind:draft,draft_id} for this client's own draft; identical identities share one job",
        handler: None,
    },
    MethodSpec {
        name: "analysis.read",
        mutates: false,
        required: &["job_id"],
        optional: &[],
        notes: "{status, identity, report?, error?}; status is pending, ready, failed, superseded or cancelled and only ready carries counts, so no state can be read as an empty histogram; a job this client did not request is a validation error",
        handler: None,
    },
    MethodSpec {
        name: "analysis.cancel",
        mutates: false,
        required: &["job_id"],
        optional: &[],
        notes: "drops this client's interest in the job and cancels the work only when no other client holds it; returns {cancelled: true}",
        handler: None,
    },
];

/// A resolved method: a host method from the static table, or one generated from a registered
/// module action or query. All three come from the same lookup discovery uses.
pub(super) enum Method {
    Host(&'static MethodSpec),
    Action(String),
    /// A module's read-only query. It writes nothing, so it never emits an event.
    Query(String),
}

impl Method {
    pub(super) fn mutates(&self) -> bool {
        match self {
            Self::Host(spec) => spec.mutates,
            Self::Action(_) => true,
            Self::Query(_) => false,
        }
    }
    /// `true` for the methods the owner loop answers from its own state.
    pub(super) fn owner_answered(&self) -> bool {
        matches!(self, Self::Host(spec) if spec.handler.is_none())
    }
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

pub(super) fn find(service: &EditorService, name: &str) -> Option<Method> {
    if let Some(spec) = METHODS.iter().find(|spec| spec.name == name) {
        return Some(Method::Host(spec));
    }
    if let Some(action_id) = name.strip_prefix("edit.") {
        return service
            .registry()
            .action(action_id)
            .map(|_| Method::Action(action_id.to_owned()));
    }
    let query_id = name.strip_prefix("query.")?;
    service
        .registry()
        .query(query_id)
        .map(|_| Method::Query(query_id.to_owned()))
}

/// A method emits an event when it is mutating and its result is not a no-op.
pub(super) fn mutates(method: &Method, result: Option<&Value>) -> bool {
    method.mutates()
        && result
            .and_then(|value| value.get("outcome"))
            .and_then(Value::as_str)
            != Some("no-op")
}

pub(super) fn dispatch(
    service: &mut EditorService,
    session: &mut ClientSession,
    request: &ApiRequest,
    sequence: u64,
) -> ApiResponse {
    let result = match find(service, &request.method) {
        Some(Method::Host(MethodSpec {
            handler: Some(handler),
            ..
        })) => handler(service, session, &request.params),
        Some(Method::Host(_)) => Err(Error::new(
            ErrorKind::Protocol,
            format!("{} is answered by the catalog owner", request.method),
        )),
        Some(Method::Action(action_id)) => {
            edit_action(service, session, &action_id, &request.params)
        }
        Some(Method::Query(query_id)) => module_query(service, session, &query_id, &request.params),
        None => Err(Error::new(
            ErrorKind::Protocol,
            format!("unknown method {}", request.method),
        )),
    };
    match result {
        Ok(result) => ApiResponse::success(request.id.clone(), sequence, result),
        Err(error) => ApiResponse::failure(request.id.clone(), sequence, error),
    }
}

/// One generated method description: the envelope every action shares plus the action's own
/// declared parameters, so a client needs no hand-maintained list.
fn action_schema(action: &ActionDescriptor) -> Value {
    let mut required = vec![json!("asset_id"), json!("mutation")];
    let mut optional = Map::new();
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

pub fn schemas(registry: &ModuleRegistry) -> Value {
    let mut methods: Map<String, Value> = METHODS
        .iter()
        .map(|spec| {
            let optional: Map<String, Value> = spec
                .optional
                .iter()
                .map(|(name, meaning)| ((*name).to_string(), json!(meaning)))
                .collect();
            (
                spec.name.to_string(),
                json!({
                    "mutates": spec.mutates,
                    "required": spec.required,
                    "optional": optional,
                    "notes": spec.notes,
                }),
            )
        })
        .collect();
    let descriptors = registry.descriptors();
    for descriptor in &descriptors {
        for action in &descriptor.actions {
            methods.insert(action_method(&action.id), action_schema(action));
        }
        for query in &descriptor.queries {
            methods.insert(query_method(&query.id), query_schema(query));
        }
    }
    json!({
        "protocol": PROTOCOL,
        "coordinate_space": "Each edit uses integer coordinates in its own input image stage after EXIF orientation. A pixel or colour edit addresses the content stage, the source after EXIF orientation, because the host places both before the quarter-turns, reflections and crop that carry them; a colour edit addresses every pixel of that stage and changes no dimension. A number parameter carries a finite JSON number within its declared range, such as an angle in degrees or a rectangle normalized to its stage; a JSON integer is accepted and passed through unchanged.",
        "methods": methods,
        "modules": descriptors,
        "mutation": {"required": ["expected_revision", "request_id", "actor"]},
    })
}

fn schema_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    _: &Value,
) -> Result<Value, Error> {
    Ok(schemas(service.registry()))
}

fn module_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: Option<AssetId>,
    }
    let p = parse::<P>(params)?;
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
    _: &Value,
) -> Result<Value, Error> {
    Ok(json!({"assets": service.assets()?}))
}

fn asset_state(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    let p = parse::<AssetParams>(params)?;
    value(service.state(&p.asset_id)?)
}

fn source_inspect(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        entry_id: Option<EntryId>,
    }
    let p = parse::<P>(params)?;
    service.inspect_source(&p.asset_id, p.entry_id.as_ref())
}

fn history_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        before_sequence: Option<u64>,
        limit: Option<usize>,
    }
    let p = parse::<P>(params)?;
    value(service.history(&p.asset_id, p.before_sequence, p.limit.unwrap_or(50))?)
}

fn history_inspect(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    let p = parse::<EntryParams>(params)?;
    value(service.entry(&p.asset_id, &p.entry_id)?)
}

fn history_lineage(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        entry_id: Option<EntryId>,
        limit: Option<usize>,
    }
    let p = parse::<P>(params)?;
    value(service.lineage(&p.asset_id, p.entry_id.as_ref(), p.limit.unwrap_or(50))?)
}

/// Every generated action method: `asset_id` and `mutation` are the envelope, the remaining
/// top-level fields are the action's declared parameters.
fn edit_action(
    service: &mut EditorService,
    session: &mut ClientSession,
    action_id: &str,
    params: &Value,
) -> Result<Value, Error> {
    require_current(session)?;
    let mut parameters = match params {
        Value::Object(object) => object.clone(),
        Value::Null => Map::new(),
        _ => {
            return Err(Error::new(
                ErrorKind::Validation,
                "params must be a JSON object",
            ));
        }
    };
    let asset_id: AssetId = envelope(&mut parameters, "asset_id")?;
    let mutation: Mutation = envelope(&mut parameters, "mutation")?;
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
    request_params: &Value,
) -> Result<Value, Error> {
    let mut parameters = match request_params {
        Value::Object(object) => object.clone(),
        Value::Null => Map::new(),
        _ => {
            return Err(Error::new(
                ErrorKind::Validation,
                "params must be a JSON object",
            ));
        }
    };
    let asset_id: AssetId = envelope(&mut parameters, "asset_id")?;
    let entry_id: EntryId = match parameters.remove("entry_id") {
        Some(entry_id) => params(&entry_id)?,
        None => match &session.preview.selection {
            HistorySelection::Current => service.state(&asset_id)?.current_entry.id,
            HistorySelection::Entry(id) => id.clone(),
        },
    };
    service.run_query(&asset_id, &entry_id, query_id, Value::Object(parameters))
}

fn envelope<T: DeserializeOwned>(
    parameters: &mut Map<String, Value>,
    name: &str,
) -> Result<T, Error> {
    let field = parameters.remove(name).ok_or_else(|| {
        Error::new(
            ErrorKind::Validation,
            format!("missing required field {name}"),
        )
    })?;
    params(&field)
}

fn history_undo(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    require_current(session)?;
    let p = parse::<MutationParams>(params)?;
    value(service.undo(&p.asset_id, p.mutation)?)
}

fn history_redo(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    require_current(session)?;
    let p = parse::<MutationParams>(params)?;
    value(service.redo(&p.asset_id, p.mutation)?)
}

fn history_restore(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        mutation: Mutation,
        entry_id: EntryId,
    }
    let p = parse::<P>(params)?;
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
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        name: String,
        actor: String,
        entry_id: Option<EntryId>,
    }
    let p = parse::<P>(params)?;
    value(service.create_version(&p.asset_id, &p.name, p.entry_id.as_ref(), &p.actor)?)
}

fn version_delete(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        name: String,
    }
    let p = parse::<P>(params)?;
    value(service.delete_version(&p.asset_id, &p.name)?)
}

fn version_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    let p = parse::<AssetParams>(params)?;
    Ok(json!({"versions": service.versions(&p.asset_id)?}))
}

fn preset_list(
    service: &mut EditorService,
    _: &mut ClientSession,
    _: &Value,
) -> Result<Value, Error> {
    Ok(json!({"presets": service.presets()?}))
}

fn preset_read(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    let p = parse::<PresetParams>(params)?;
    let (record, source_text) = service.preset(&p.preset_id)?;
    let mut preset = value(record)?;
    preset["source_text"] = json!(source_text);
    Ok(json!({"preset": preset}))
}

fn preset_create(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        name: String,
        settings: Map<String, Value>,
        actor: String,
        group: Option<String>,
    }
    let p = parse::<P>(params)?;
    let preset = service.create_preset(&p.name, p.group.as_deref(), &p.settings, &p.actor)?;
    Ok(json!({"preset": preset}))
}

/// Capture reads the entry the caller names, or the session's selection exactly as `render.sample`
/// resolves it, so the desktop captures the entry it displays.
fn preset_capture(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        fields: Map<String, Value>,
        entry_id: Option<EntryId>,
    }
    let p = parse::<P>(params)?;
    let entry_id = match p.entry_id {
        Some(entry_id) => entry_id,
        None => match &session.preview.selection {
            HistorySelection::Current => service.state(&p.asset_id)?.current_entry.id,
            HistorySelection::Entry(id) => id.clone(),
        },
    };
    Ok(json!({"settings": service.capture_preset(&p.asset_id, &entry_id, &p.fields)?}))
}

fn preset_update(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        preset_id: PresetId,
        actor: String,
        name: Option<String>,
        group: Option<String>,
        settings: Option<Map<String, Value>>,
    }
    let p = parse::<P>(params)?;
    value(service.update_preset(
        &p.preset_id,
        &p.actor,
        p.name.as_deref(),
        p.group.as_deref(),
        p.settings.as_ref(),
    )?)
}

fn preset_delete(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    let p = parse::<PresetParams>(params)?;
    let outcome = service.delete_preset(&p.preset_id)?;
    Ok(json!({"outcome": outcome, "deleted": outcome == MutationOutcome::Applied}))
}

fn preset_export(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    let p = parse::<PresetParams>(params)?;
    value(service.export_preset(&p.preset_id)?)
}

fn preset_inspect(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        content: String,
        file_name: Option<String>,
    }
    let p = parse::<P>(params)?;
    service.inspect_import(&p.content, p.file_name.as_deref())
}

fn preset_import(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        content: String,
        actor: String,
        file_name: Option<String>,
        name: Option<String>,
        group: Option<String>,
    }
    let p = parse::<P>(params)?;
    let preset = service.import_preset(
        &p.content,
        p.file_name.as_deref(),
        p.name.as_deref(),
        p.group.as_deref(),
        &p.actor,
    )?;
    Ok(json!({"report": preset.report, "preset": preset}))
}

fn preview_select(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    let p = parse::<EntryParams>(params)?;
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
    _: &Value,
) -> Result<Value, Error> {
    let generation = session.preview.return_current();
    session.touch();
    Ok(json!({"generation": generation, "session": session_value(service, session)?}))
}

fn view_set(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        zoom: Option<Zoom>,
        pan_x: Option<f32>,
        pan_y: Option<f32>,
    }
    let p = parse::<P>(params)?;
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
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        entry_id: Option<EntryId>,
    }
    let p = parse::<P>(params)?;
    value(service.describe_entry(&p.asset_id, p.entry_id.as_ref())?)
}

/// The canvas modes this registry offers: the pointer plus every available module that declares a
/// canvas interaction. `O(modules)`; it touches no image resource.
fn canvas_modes(registry: &ModuleRegistry) -> Vec<String> {
    let mut modes = vec![POINTER_MODE.to_owned()];
    modes.extend(
        registry
            .descriptors()
            .iter()
            .filter(|descriptor| descriptor.is_available() && descriptor.canvas.is_some())
            .map(|descriptor| descriptor.id.clone()),
    );
    modes
}

/// A normal `Option<Option<T>>` deserializer cannot distinguish a missing field from explicit
/// JSON null. `workspace.set` needs that distinction: omission preserves, null closes the board.
fn present_nullable_page<'de, D>(deserializer: D) -> Result<Option<Option<usize>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<usize>::deserialize(deserializer).map(Some)
}

fn workspace_set(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        state_panel: Option<bool>,
        tools_panel: Option<bool>,
        mode: Option<String>,
        thirds: Option<bool>,
        clip_shadows: Option<bool>,
        clip_highlights: Option<bool>,
        #[serde(default, deserialize_with = "present_nullable_page")]
        component_gallery: Option<Option<usize>>,
    }
    let p = parse::<P>(params)?;
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
    if let Some(page) = p.component_gallery {
        session.workspace.component_gallery = page;
    }
    session.touch();
    session_value(service, session)
}

fn session_state(
    service: &mut EditorService,
    session: &mut ClientSession,
    _: &Value,
) -> Result<Value, Error> {
    session_value(service, session)
}

fn render_sample(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        x: u32,
        y: u32,
        draft_id: Option<DraftId>,
    }
    let p = parse::<P>(params)?;
    let mut sampled = match &p.draft_id {
        // A draft's effective recipe answers the point, so a readout during a gesture matches the
        // frame the same draft is previewing.
        Some(draft_id) => {
            let draft = held_draft(session, draft_id)?.clone();
            value(service.sample_draft(&p.asset_id, &draft, p.x, p.y)?)?
        }
        None => {
            let entry_id = match &session.preview.selection {
                HistorySelection::Current => service.state(&p.asset_id)?.current_entry.id,
                HistorySelection::Entry(id) => id.clone(),
            };
            value(service.sample_entry(&p.asset_id, &entry_id, p.x, p.y)?)?
        }
    };
    sampled["source_detail_ready"] = json!(true);
    Ok(sampled)
}

/// The draft this client holds under that identity. Another client's draft, or one that has
/// already ended, is simply not this session's.
fn held_draft<'a>(session: &'a ClientSession, draft_id: &DraftId) -> Result<&'a Draft, Error> {
    session
        .draft
        .as_ref()
        .filter(|draft| &draft.draft_id == draft_id)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::Validation,
                format!("unknown draft {draft_id} for this client"),
            )
        })
}

/// `conflicted` is derived, never notified: the asset moved under the draft. Every read, set,
/// commit and session report recomputes it from the asset's current revision.
fn refresh_conflict(service: &EditorService, session: &mut ClientSession) -> Result<(), Error> {
    if let Some(draft) = &mut session.draft {
        draft.conflicted = draft.base_revision != service.state(&draft.asset_id)?.revision;
    }
    Ok(())
}

/// The session as a client reads it, with its draft's conflict state recomputed first.
fn session_value(service: &EditorService, session: &mut ClientSession) -> Result<Value, Error> {
    refresh_conflict(service, session)?;
    value(session)
}

/// The action a draft will run, and the parameters its fields are validated against.
fn draft_action<'a>(
    service: &'a EditorService,
    action_id: &str,
) -> Result<&'a ActionDescriptor, Error> {
    service
        .registry()
        .action(action_id)
        .map(|(_, action)| action)
        .ok_or_else(|| Error::new(ErrorKind::Validation, format!("unknown action {action_id}")))
}

fn draft_begin(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        action: String,
    }
    let p = parse::<P>(params)?;
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
    let revision = service.state(&p.asset_id)?.revision;
    let draft = Draft::new(&p.action, p.asset_id, revision);
    session.draft = Some(draft);
    session.touch();
    value(session.draft.as_ref().expect("the draft just opened"))
}

fn draft_set(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        draft_id: DraftId,
        fields: Map<String, Value>,
    }
    let p = parse::<P>(params)?;
    let draft = held_draft(session, &p.draft_id)?;
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
    params: &Value,
) -> Result<Value, Error> {
    let p = parse::<DraftParams>(params)?;
    held_draft(session, &p.draft_id)?;
    draft_value(service, session)
}

fn draft_cancel(
    _: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    let p = parse::<DraftParams>(params)?;
    held_draft(session, &p.draft_id)?;
    session.draft = None;
    session.touch();
    Ok(json!({"cancelled": true}))
}

fn draft_commit(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        draft_id: DraftId,
        mutation: Mutation,
    }
    let p = parse::<P>(params)?;
    refresh_conflict(service, session)?;
    let draft = held_draft(session, &p.draft_id)?;
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
    let fields = Value::Object(draft.fields.clone());
    // A failed commit keeps the draft, so the client can correct it and try again; a no-op ends it
    // exactly like an applied one, because the gesture is over either way.
    let result = service.apply_action(&asset_id, p.mutation, &action, fields)?;
    session.draft = None;
    session.touch();
    value(result)
}

fn draft_reapply(
    service: &mut EditorService,
    session: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    let p = parse::<DraftParams>(params)?;
    let draft = held_draft(session, &p.draft_id)?;
    // Only the fields this client set survive, revalidated against the action they belong to;
    // whatever another client changed meanwhile stays in the layer the commit merges over.
    let action = draft_action(service, &draft.action)?;
    draft.checked_fields(&action.parameters, &draft.fields.clone())?;
    let revision = service.state(&draft.asset_id)?.revision;
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
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        asset_id: AssetId,
        entry_id: Option<EntryId>,
        x: u32,
        y: u32,
    }
    let p = parse::<P>(params)?;
    let entry_id = match p.entry_id {
        Some(entry_id) => entry_id,
        None => match &session.preview.selection {
            HistorySelection::Current => service.state(&p.asset_id)?.current_entry.id,
            HistorySelection::Entry(id) => id.clone(),
        },
    };
    value(service.locate_entry(&p.asset_id, &entry_id, p.x, p.y)?)
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssetParams {
    asset_id: AssetId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryParams {
    asset_id: AssetId,
    entry_id: EntryId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MutationParams {
    asset_id: AssetId,
    mutation: Mutation,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftParams {
    draft_id: DraftId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetParams {
    preset_id: PresetId,
}

fn parse<T: DeserializeOwned>(value: &Value) -> Result<T, Error> {
    params(value)
}
pub(super) fn params<T: DeserializeOwned>(value: &Value) -> Result<T, Error> {
    serde_json::from_value(value.clone())
        .map_err(|error| Error::new(ErrorKind::Validation, error.to_string()))
}
fn value(value: impl Serialize) -> Result<Value, Error> {
    serde_json::to_value(value).map_err(|error| Error::new(ErrorKind::Internal, error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ActionInput, ActionPlan, Availability, EFFECT_FORMAT, EffectDescriptor, EffectStage,
        ExactGeometry, Layer, LayerId, ModuleDescriptor, ParameterDescriptor, ParameterKind,
        Processing, Stage, StageContext, ToolModule,
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

    fn call(
        service: &mut EditorService,
        session: &mut ClientSession,
        method: &str,
        params: Value,
    ) -> ApiResponse {
        dispatch(
            service,
            session,
            &ApiRequest {
                id: method.into(),
                method: method.into(),
                params,
                token: None,
            },
            0,
        )
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
        let schema = schemas(service.registry());
        let listed = schema["methods"].as_object().unwrap();
        assert_eq!(
            listed.len(),
            METHODS.len() + generated.len() + queries.len()
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
        assert_eq!(
            listed["recipe.describe"],
            json!({
                "mutates": false,
                "required": ["asset_id"],
                "optional": {"entry_id": "entry to describe; default current"},
                "notes": listed["recipe.describe"]["notes"],
            })
        );
        // The preset library's host methods, in table order, with their parameters.
        assert_eq!(
            METHODS
                .iter()
                .map(|spec| spec.name)
                .filter(|name| name.starts_with("preset."))
                .collect::<Vec<_>>(),
            [
                "preset.list",
                "preset.read",
                "preset.create",
                "preset.capture",
                "preset.update",
                "preset.delete",
                "preset.export",
                "preset.inspect",
                "preset.import",
            ]
        );
        for (name, mutates, required, optional) in [
            ("preset.list", false, json!([]), vec![]),
            ("preset.read", false, json!(["preset_id"]), vec![]),
            (
                "preset.create",
                true,
                json!(["name", "settings", "actor"]),
                vec!["group"],
            ),
            (
                "preset.capture",
                false,
                json!(["asset_id", "fields"]),
                vec!["entry_id"],
            ),
            (
                "preset.update",
                true,
                json!(["preset_id", "actor"]),
                vec!["group", "name", "settings"],
            ),
            ("preset.delete", true, json!(["preset_id"]), vec![]),
            ("preset.export", false, json!(["preset_id"]), vec![]),
            (
                "preset.inspect",
                false,
                json!(["content"]),
                vec!["file_name"],
            ),
            (
                "preset.import",
                true,
                json!(["content", "actor"]),
                vec!["file_name", "group", "name"],
            ),
        ] {
            assert_eq!(listed[name]["mutates"], json!(mutates), "{name}");
            assert_eq!(listed[name]["required"], required, "{name}");
            assert_eq!(
                listed[name]["optional"]
                    .as_object()
                    .expect("the optional fields")
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                optional,
                "{name}"
            );
        }
        assert_eq!(listed["workspace.set"]["required"], json!([]));
        assert_eq!(
            listed["workspace.set"]["optional"]
                .as_object()
                .expect("the workspace fields")
                .keys()
                .collect::<Vec<_>>(),
            [
                "clip_highlights",
                "clip_shadows",
                "component_gallery",
                "mode",
                "state_panel",
                "thirds",
                "tools_panel"
            ]
        );
        // The descriptor additions the workspace renders from reach a client through module.list.
        let modules = module_list(&mut service, &mut session, &json!({})).unwrap();
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
            let request = ApiRequest {
                id: "schema".into(),
                method: name.into(),
                params: json!({}),
                token: None,
            };
            let response = dispatch(&mut service, &mut session, &request, 0);
            if let Some(error) = &response.error {
                assert!(
                    !error.message.starts_with("unknown method"),
                    "{name} is listed but not dispatched"
                );
                if spec.owner_answered() {
                    assert_eq!(error.code, "protocol");
                }
            }
            assert!(
                !mutates(&spec, Some(&json!({"outcome": "no-op"}))),
                "{name} must not emit events for a no-op"
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
        let response = dispatch(
            &mut service,
            &mut session,
            &ApiRequest {
                id: "angle".into(),
                method: "edit.test-angle".into(),
                params: json!({
                    "asset_id": asset,
                    "mutation": {"expected_revision":0,"request_id":"angle","actor":"test"},
                    "angle": -3.5,
                }),
                token: None,
            },
            0,
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
            Ok(ActionPlan::Commit(Layer {
                id: LayerId::new(),
                effect_id: MARK_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({}),
            }))
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
                // The accepted modes are derived from the registry's canvas declarations, so the
                // RAW and Basic neutral pickers join the list without a change here.
                "mode must be one of pointer, lightwell.pixel, lightwell.raw, lightwell.basic, lightwell.crop",
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
