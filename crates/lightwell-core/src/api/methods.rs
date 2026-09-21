//! The method table: host methods carry their schema description, mutation flag and handler, and
//! every module action resolves to a generated `edit.<action>` method from the same registry, so
//! discovery, event emission and dispatch cannot drift apart.
use super::{ApiRequest, ApiResponse, ClientSession, POINTER_MODE, PROTOCOL};
use crate::{
    ActionDescriptor, AssetId, EditorService, EntryId, Error, ErrorKind, HistorySelection,
    ModuleRegistry, Mutation, Zoom,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

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
        notes: "references an existing JPEG without copying it and returns the asset state",
        handler: Some(catalog_import),
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
        optional: &[],
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
        ],
        notes: "session workspace state: panels, canvas mode and the thirds overlay; returns the session",
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
        name: "render.sample",
        mutates: false,
        required: &["asset_id", "x", "y"],
        optional: &[],
        notes: "one pixel of the session's selected entry, evaluated without rasterizing",
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
];

/// A resolved method: a host method from the static table, or one generated from a registered
/// module action. Both come from the same lookup discovery uses.
pub(super) enum Method {
    Host(&'static MethodSpec),
    Action(String),
}

impl Method {
    pub(super) fn mutates(&self) -> bool {
        match self {
            Self::Host(spec) => spec.mutates,
            Self::Action(_) => true,
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

pub(super) fn find(service: &EditorService, name: &str) -> Option<Method> {
    if let Some(spec) = METHODS.iter().find(|spec| spec.name == name) {
        return Some(Method::Host(spec));
    }
    let action_id = name.strip_prefix("edit.")?;
    service
        .registry()
        .action(action_id)
        .map(|_| Method::Action(action_id.to_owned()))
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
        if parameter.required && parameter.default.is_none() {
            required.push(json!(parameter.name));
        } else {
            optional.insert(parameter.name.clone(), json!(parameter.notes));
        }
    }
    json!({
        "mutates": true,
        "required": required,
        "optional": optional,
        "notes": action.notes,
        "parameters": action.parameters,
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
    }
    json!({
        "protocol": PROTOCOL,
        "coordinate_space": "Each edit uses integer coordinates in its own input image stage after EXIF orientation. A pixel edit addresses the content stage, the source after EXIF orientation, because the host places it before the quarter-turns, reflections and crop that carry it. A number parameter carries a finite JSON number within its declared range, such as an angle in degrees or a rectangle normalized to its stage; a JSON integer is accepted and passed through unchanged.",
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
    _: &Value,
) -> Result<Value, Error> {
    Ok(json!({"modules": service.registry().descriptors()}))
}

fn catalog_import(
    service: &mut EditorService,
    _: &mut ClientSession,
    params: &Value,
) -> Result<Value, Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct P {
        path: PathBuf,
    }
    value(service.import(&parse::<P>(params)?.path)?)
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
    Ok(json!({"generation": generation, "session": session}))
}

fn preview_return_current(
    _: &mut EditorService,
    session: &mut ClientSession,
    _: &Value,
) -> Result<Value, Error> {
    let generation = session.preview.return_current();
    session.touch();
    Ok(json!({"generation": generation, "session": session}))
}

fn view_set(
    _: &mut EditorService,
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
    value(session)
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
    session.touch();
    value(session)
}

fn session_state(
    _: &mut EditorService,
    session: &mut ClientSession,
    _: &Value,
) -> Result<Value, Error> {
    value(session)
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
    }
    let p = parse::<P>(params)?;
    let entry_id = match &session.preview.selection {
        HistorySelection::Current => service.state(&p.asset_id)?.current_entry.id,
        HistorySelection::Entry(id) => id.clone(),
    };
    let mut sampled = value(service.sample_entry(&p.asset_id, &entry_id, p.x, p.y)?)?;
    sampled["source_detail_ready"] = json!(true);
    Ok(sampled)
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
                "edit.set-pixel",
                "edit.transform",
                "edit.crop",
                "edit.crop-fit",
                "edit.crop-reset"
            ]
        );
        let schema = schemas(service.registry());
        let listed = schema["methods"].as_object().unwrap();
        assert_eq!(listed.len(), METHODS.len() + generated.len());
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
        assert_eq!(listed["workspace.set"]["required"], json!([]));
        assert_eq!(
            listed["workspace.set"]["optional"]
                .as_object()
                .expect("the workspace fields")
                .keys()
                .collect::<Vec<_>>(),
            ["mode", "state_panel", "thirds", "tools_panel"]
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
                }],
                actions: vec![ActionDescriptor {
                    id: "test-angle".into(),
                    title: "Set angle".into(),
                    notes: "test".into(),
                    summary: Some("Angle {angle}".into()),
                    parameters: vec![ParameterDescriptor {
                        name: "angle".into(),
                        kind: ParameterKind::Number {
                            min: -45.0,
                            max: 45.0,
                        },
                        required: true,
                        default: None,
                        unit: Some("deg".into()),
                        notes: "test".into(),
                    }],
                }],
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
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
                }],
                actions: vec![ActionDescriptor {
                    id: MARK_ACTION.into(),
                    title: "Mark".into(),
                    notes: "test".into(),
                    summary: None,
                    parameters: Vec::new(),
                }],
                controls: Vec::new(),
                reset: None,
                canvas: None,
                developer: false,
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
            asset = ok(
                &mut service,
                &mut session,
                "catalog.import",
                json!({"path": fixture()}),
            )["asset"]["id"]
                .clone();
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
            json!({"state_panel": true, "tools_panel": true, "mode": "pointer", "thirds": false}),
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
            json!({"state_panel": false, "tools_panel": true, "mode": "lightwell.crop", "thirds": true})
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
                "mode must be one of pointer, lightwell.pixel, lightwell.crop",
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
}
