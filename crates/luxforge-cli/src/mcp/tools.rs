//! The tools the adapter offers ([tools](../../../../docs/design/mcp.md#tools)), all drawn from
//! the registry's `schema.list` and its markers, never from a list of methods of its own.
//!
//! [`Profile::Compact`] offers three fixed tools whatever the session: `luxforge_methods` lists the
//! registry, `luxforge_describe` gives methods' generated input schemas and notes, and
//! `luxforge_call` calls any method a live client may call, through [`call`]. A typed profile, one
//! tool per method, is another [`Profile`] over the same registry reading and the same [`call`].
use super::{
    images,
    owner::{Owner, Session},
    protocol::{ClientInfo, INVALID_PARAMS, RpcError},
    schema::{EXPECTED_REVISION, ExpectedRevision, MethodInput, MethodInputs, SchemaError},
    serve::Cancel,
};
use crate::live::{CONNECTION_LOST, JOB_RELEASED, OUTCOME_UNKNOWN, PROTOCOL_ERROR, failure};
use luxforge_core::{ApiFailure, Envelope, ModuleRegistry, Mutation, MutationRequest, schemas};
use serde_json::{Map, Value, json};
use std::sync::OnceLock;

/// The tool that lists the registry's methods.
pub const METHODS: &str = "luxforge_methods";
/// The tool that describes methods.
pub const DESCRIBE: &str = "luxforge_describe";
/// The tool that calls a method.
pub const CALL: &str = "luxforge_call";

/// The most methods one `luxforge_describe` describes.
pub const DESCRIBE_LIMIT: usize = 16;
/// The longest summary `luxforge_methods` gives a method, in characters.
const SUMMARY_LIMIT: usize = 240;
/// The longest filter `luxforge_methods` takes, in characters.
const FILTER_LIMIT: usize = 256;
/// The markers an entry passes through to `luxforge_methods` and `luxforge_describe` as the
/// registry gives them, when present.
const MARKERS: [&str; 3] = ["read_only", "reversible", "network"];

/// What a session-scoped method's state lives as long as, said wherever such a method is listed.
const SESSION_NOTE: &str = "session-scoped: it acts on this adapter's own connection to Luxforge, \
                            which lasts as long as this luxforge-mcp process stays attached, so \
                            what it begins carries from one luxforge_call to the next; when the \
                            connection ends (Luxforge quits or restarts) it is gone, and the next \
                            result warns session-replaced";

/// Which tools are offered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    /// `luxforge_methods`, `luxforge_describe` and `luxforge_call`: a fixed list.
    Compact,
}

impl Profile {
    /// Whether the tool list can change while the server runs, so `listChanged` is advertised.
    pub fn list_changed(self) -> bool {
        match self {
            Self::Compact => false,
        }
    }

    /// The tools, in a fixed order.
    pub fn tools(self) -> Vec<Value> {
        match self {
            Self::Compact => compact_tools(),
        }
    }
}

/// What a tool call came to: its structured result, or a failure as `{error}`, with the pictures
/// and warnings beside it.
#[derive(Debug, Default)]
pub struct ToolOutcome {
    pub value: Value,
    pub is_error: bool,
    /// base64-encoded JPEGs.
    pub images: Vec<String>,
    pub warnings: Vec<ApiFailure>,
}

impl ToolOutcome {
    fn ok(value: Value, warnings: Vec<ApiFailure>) -> Self {
        Self {
            value,
            warnings,
            ..Self::default()
        }
    }

    fn failed(error: ApiFailure, warnings: Vec<ApiFailure>) -> Self {
        Self {
            value: json!({"error": error}),
            is_error: true,
            warnings,
            ..Self::default()
        }
    }
}

/// The work stopped at a hold because the client cancelled the request: nothing is answered.
pub struct Cancelled;

/// How the tools are served for one launch.
#[derive(Clone, Copy, Debug)]
pub struct Tools {
    pub profile: Profile,
    /// Whether marked pictures are attached (`--no-images` turns it off).
    pub images: bool,
}

impl Tools {
    /// Call tool `name` with `arguments` for `client`. An unknown tool is a protocol error; every
    /// other failure is the tool's own, in its outcome.
    pub fn call(
        &self,
        owner: &mut Owner,
        client: &ClientInfo,
        name: &str,
        arguments: &Map<String, Value>,
        cancel: &Cancel,
    ) -> Result<Result<ToolOutcome, Cancelled>, RpcError> {
        match (self.profile, name) {
            (Profile::Compact, METHODS) => Ok(Ok(methods(owner, client, arguments))),
            (Profile::Compact, DESCRIBE) => Ok(Ok(describe(owner, client, arguments))),
            (Profile::Compact, CALL) => {
                Ok(call_tool(owner, client, arguments, cancel, self.images))
            }
            _ => Err(
                RpcError::new(INVALID_PARAMS, format!("Unknown tool: {name}"))
                    .with_data(json!({"tool": name})),
            ),
        }
    }
}

fn compact_tools() -> Vec<Value> {
    let read_only = json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
    });
    vec![
        json!({
            "name": METHODS,
            "title": "List Luxforge methods",
            "description": "Lists the operations of the running Luxforge photo editor's registry, \
                in registry order: each method's name, a one-line summary (the first clause of its \
                notes), mutates, session_scoped, its effect markers (read_only, reversible, \
                network) as the registry gives them, and callable, false for a method only the \
                desktop itself may call. namespace keeps the methods whose name is it or starts \
                with it and a dot (edit, asset, history, job, preview, draft, catalog...); query \
                keeps those whose name or notes contain it, ignoring case. While Luxforge is not \
                running it answers from the registry built into this adapter, with source: \
                built-in and why; the running editor's registry, source: live, can differ. Next: \
                luxforge_describe for parameters, then luxforge_call.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "namespace": {"type": "string", "maxLength": FILTER_LIMIT,
                        "description": "a method name's leading segments, such as edit or module.settings"},
                    "query": {"type": "string", "maxLength": FILTER_LIMIT,
                        "description": "text a method's name or notes contain, ignoring case"},
                },
                "additionalProperties": false,
            },
            "annotations": read_only,
        }),
        json!({
            "name": DESCRIBE,
            "title": "Describe Luxforge methods",
            "description": "Describes up to 16 Luxforge methods: for each, its full notes, \
                input_schema (the JSON Schema of the params luxforge_call sends it), envelope \
                (revision, request or null: luxforge_call builds it, never pass mutation), \
                expected_revision (optional for an asset's revision, which luxforge_call reads \
                from asset.state when it is left out; required for a draft's or module settings' \
                revision; absent when the method has none), its markers and callable. A \
                session-scoped method acts on this adapter's own connection to Luxforge, which \
                lasts as long as this server stays attached. Answers from the built-in registry, \
                source: built-in, while Luxforge is not running.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "methods": {
                        "type": "array",
                        "items": {"type": "string", "maxLength": FILTER_LIMIT},
                        "minItems": 1,
                        "maxItems": DESCRIBE_LIMIT,
                        "description": "method names, as luxforge_methods lists them",
                    },
                },
                "required": ["methods"],
                "additionalProperties": false,
            },
            "annotations": read_only,
        }),
        json!({
            "name": CALL,
            "title": "Call a Luxforge method",
            "description": "Calls one method of the running Luxforge desktop's registry as an \
                ordinary live client with edit authority: the same operation a person performs, \
                an edit recorded in history under this agent's name and undoable there. params \
                are checked against the method's input_schema (luxforge_describe) before anything \
                is sent. The adapter builds the mutation envelope itself; for a method whose \
                revision is an asset's it reads asset.state just before the edit unless \
                expected_revision is given, and a draft's or module settings' expected_revision \
                must be given. A Luxforge failure such as validation or conflict is an error \
                result {error: {code, message, data?, job_id?}}, and nothing is ever retried. A \
                result naming a job is followed with job.wait (each call holds at most 30 s); a \
                job owned by its clients stops when this connection ends. preview.read, and a \
                preview job's result read with job.read or job.wait, attach the picture. \
                Session-scoped methods (draft.*, preview.select, view.set and the rest) act on \
                this adapter's own connection, which lasts as long as this server stays attached. \
                A method only the desktop may call, such as a permission grant, is refused.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "method": {"type": "string", "maxLength": FILTER_LIMIT,
                        "description": "the method's name, such as edit.set-basic"},
                    "params": {"type": "object",
                        "description": "the method's parameters, as its input_schema describes them, without mutation"},
                    EXPECTED_REVISION: {"type": "integer", "minimum": 0,
                        "description": "the revision the change is checked against: an asset's, as asset.state answers it, or a draft's or module settings'"},
                },
                "required": ["method"],
                "additionalProperties": false,
            },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": true,
                "idempotentHint": false,
                "openWorldHint": true,
            },
        }),
    ]
}

fn validation(message: impl Into<String>, parameter: Option<&str>) -> ApiFailure {
    failure(
        "validation",
        message,
        parameter.map(|parameter| json!({"parameter": parameter})),
    )
}

/// Refuse an argument a tool does not take, naming it.
fn only(arguments: &Map<String, Value>, tool: &str, taken: &[&str]) -> Result<(), ApiFailure> {
    match arguments
        .keys()
        .find(|name| !taken.contains(&name.as_str()))
    {
        Some(name) => Err(validation(
            format!(
                "{tool} takes no argument {name}; it takes {}",
                taken.join(", ")
            ),
            Some(name),
        )),
        None => Ok(()),
    }
}

/// A string argument of at most [`FILTER_LIMIT`] characters, if given.
fn text<'a>(arguments: &'a Map<String, Value>, name: &str) -> Result<Option<&'a str>, ApiFailure> {
    match arguments.get(name) {
        None => Ok(None),
        Some(Value::String(text)) if text.chars().count() <= FILTER_LIMIT => Ok(Some(text)),
        Some(_) => Err(validation(
            format!("{name} is a string of at most {FILTER_LIMIT} characters"),
            Some(name),
        )),
    }
}

/// The registry built into this adapter, read once.
struct BuiltIn {
    schema: Value,
    inputs: Result<MethodInputs, SchemaError>,
}

fn built_in() -> &'static BuiltIn {
    static BUILT_IN: OnceLock<BuiltIn> = OnceLock::new();
    BUILT_IN.get_or_init(|| {
        let schema = schemas(&ModuleRegistry::builtin());
        let inputs = MethodInputs::read(&schema);
        BuiltIn { schema, inputs }
    })
}

/// The registry a listing reads: the live session's, or the built-in one and why there is no
/// session.
enum Registry<'a> {
    Live(&'a Session),
    BuiltIn(&'static BuiltIn, ApiFailure),
}

impl Registry<'_> {
    fn schema(&self) -> &Value {
        match self {
            Self::Live(session) => &session.schema,
            Self::BuiltIn(built_in, _) => &built_in.schema,
        }
    }

    fn inputs(&self) -> &Result<MethodInputs, SchemaError> {
        match self {
            Self::Live(session) => &session.inputs,
            Self::BuiltIn(built_in, _) => &built_in.inputs,
        }
    }

    /// `{source, catalog}` for a live registry, `{source, session}` for the built-in one.
    fn source(&self) -> Map<String, Value> {
        let mut source = Map::new();
        match self {
            Self::Live(session) => {
                source.insert("source".into(), json!("live"));
                source.insert("catalog".into(), json!(session.catalog));
            }
            Self::BuiltIn(_, why) => {
                source.insert("source".into(), json!("built-in"));
                source.insert("session".into(), json!(why));
            }
        }
        source
    }
}

/// The live registry when a session can be attached, else the built-in one.
fn registry<'a>(
    owner: &'a mut Owner,
    client: &ClientInfo,
    warnings: &mut Vec<ApiFailure>,
) -> Registry<'a> {
    match owner.session(&client.label()) {
        Ok((session, warning)) => {
            warnings.extend(warning);
            Registry::Live(session)
        }
        Err(why) => Registry::BuiltIn(built_in(), why),
    }
}

/// The first clause of a method's notes, at most [`SUMMARY_LIMIT`] characters.
fn summary(notes: &str) -> String {
    let end = [". ", "; "]
        .iter()
        .filter_map(|separator| notes.find(separator))
        .min()
        .unwrap_or(notes.len());
    let clause = notes[..end].trim().trim_end_matches('.');
    if clause.chars().count() <= SUMMARY_LIMIT {
        return clause.to_owned();
    }
    let mut bounded: String = clause.chars().take(SUMMARY_LIMIT - 1).collect();
    bounded.push('…');
    bounded
}

/// What every listing says of a method: its effects as the registry marks them, and whether a live
/// client may call it at all.
fn markers(entry: &Value, into: &mut Map<String, Value>) {
    into.insert("mutates".into(), json!(entry["mutates"] == json!(true)));
    into.insert(
        "session_scoped".into(),
        json!(entry["session_scoped"] == json!(true)),
    );
    for marker in MARKERS {
        if let Some(value) = entry.get(marker) {
            into.insert(marker.into(), value.clone());
        }
    }
    into.insert("callable".into(), json!(callable(entry)));
}

/// Whether a live client may call the method at all: not one only permission authority may.
fn callable(entry: &Value) -> bool {
    entry["authority"] != json!("permissions")
}

fn methods(owner: &mut Owner, client: &ClientInfo, arguments: &Map<String, Value>) -> ToolOutcome {
    let mut warnings = Vec::new();
    let filters = only(arguments, METHODS, &["namespace", "query"])
        .and_then(|()| Ok((text(arguments, "namespace")?, text(arguments, "query")?)));
    let (namespace, query) = match filters {
        Ok(filters) => filters,
        Err(error) => return ToolOutcome::failed(error, warnings),
    };
    let query = query.map(str::to_lowercase);
    let registry = registry(owner, client, &mut warnings);
    let mut listed = Vec::new();
    if let Some(methods) = registry.schema()["methods"].as_object() {
        for (name, entry) in methods {
            let notes = entry["notes"].as_str().unwrap_or_default();
            if let Some(namespace) = namespace
                && name != namespace
                && !name
                    .strip_prefix(namespace)
                    .is_some_and(|rest| rest.starts_with('.'))
            {
                continue;
            }
            if let Some(query) = &query
                && !name.to_lowercase().contains(query)
                && !notes.to_lowercase().contains(query)
            {
                continue;
            }
            let mut listing = Map::new();
            listing.insert("name".into(), json!(name));
            listing.insert("summary".into(), json!(summary(notes)));
            markers(entry, &mut listing);
            listed.push(Value::Object(listing));
        }
    }
    let mut answer = registry.source();
    answer.insert("count".into(), json!(listed.len()));
    answer.insert("methods".into(), Value::Array(listed));
    ToolOutcome::ok(Value::Object(answer), warnings)
}

/// A method's generated input, as `luxforge_call`'s `params` take it: without `expected_revision`,
/// which `luxforge_call` takes beside them.
fn params_schema(input: &MethodInput) -> Value {
    let mut schema = input.input_schema();
    if let Some(properties) = schema["properties"].as_object_mut() {
        properties.remove(EXPECTED_REVISION);
    }
    if let Some(required) = schema["required"].as_array_mut() {
        required.retain(|name| name != EXPECTED_REVISION);
    }
    schema
}

fn describe(owner: &mut Owner, client: &ClientInfo, arguments: &Map<String, Value>) -> ToolOutcome {
    let mut warnings = Vec::new();
    if let Err(error) = only(arguments, DESCRIBE, &["methods"]) {
        return ToolOutcome::failed(error, warnings);
    }
    let names: Option<Vec<&str>> = arguments
        .get("methods")
        .and_then(Value::as_array)
        .and_then(|names| names.iter().map(Value::as_str).collect());
    let names = match names {
        Some(names) if (1..=DESCRIBE_LIMIT).contains(&names.len()) => names,
        _ => {
            return ToolOutcome::failed(
                validation(
                    format!("methods is a list of 1 to {DESCRIBE_LIMIT} method names"),
                    Some("methods"),
                ),
                warnings,
            );
        }
    };
    let registry = registry(owner, client, &mut warnings);
    let inputs = match registry.inputs() {
        Ok(inputs) => inputs,
        Err(error) => {
            return ToolOutcome::failed(
                failure(
                    PROTOCOL_ERROR,
                    format!("this adapter cannot offer the registry: {error}"),
                    None,
                ),
                warnings,
            );
        }
    };
    let unknown: Vec<&str> = names
        .iter()
        .copied()
        .filter(|name| inputs.get(name).is_none())
        .collect();
    if !unknown.is_empty() {
        return ToolOutcome::failed(
            validation(
                format!(
                    "unknown methods {}; luxforge_methods lists them",
                    unknown.join(", ")
                ),
                Some("methods"),
            ),
            warnings,
        );
    }
    let mut described = Vec::new();
    for name in names {
        if described.iter().any(|seen: &Value| seen["name"] == name) {
            continue;
        }
        let input = inputs.get(name).expect("checked above");
        let entry = &registry.schema()["methods"][name];
        let mut description = Map::new();
        description.insert("name".into(), json!(name));
        description.insert("notes".into(), entry["notes"].clone());
        markers(entry, &mut description);
        description.insert("envelope".into(), json!(input.envelope().name()));
        if let Some(of) = input.revision_of() {
            description.insert("revision_of".into(), json!(of));
        }
        match input.expected_revision() {
            ExpectedRevision::None => {}
            ExpectedRevision::Optional => {
                description.insert(EXPECTED_REVISION.into(), json!("optional"));
            }
            ExpectedRevision::Required => {
                description.insert(EXPECTED_REVISION.into(), json!("required"));
            }
        }
        if entry["session_scoped"] == json!(true) {
            description.insert("session".into(), json!(SESSION_NOTE));
        }
        description.insert("input_schema".into(), params_schema(input));
        described.push(Value::Object(description));
    }
    let mut answer = registry.source();
    answer.insert("methods".into(), Value::Array(described));
    ToolOutcome::ok(Value::Object(answer), warnings)
}

/// A new envelope with new request identities and `actor`: a revision envelope at
/// `expected_revision` when there is one, else a request envelope.
fn envelope(expected_revision: Option<u64>, actor: &str) -> (Value, String) {
    let request_id = uuid::Uuid::new_v4().to_string();
    let actor = actor.to_owned();
    let built = match expected_revision {
        Some(expected_revision) => serde_json::to_value(Mutation {
            expected_revision,
            request_id: request_id.clone(),
            actor,
        }),
        None => serde_json::to_value(MutationRequest {
            request_id: request_id.clone(),
            actor,
        }),
    };
    (built.expect("an envelope serializes"), request_id)
}

/// `luxforge_call`'s own arguments.
struct CallArguments<'a> {
    method: &'a str,
    params: Map<String, Value>,
    /// The revision given beside the parameters.
    expected_revision: Option<&'a Value>,
}

/// Read `luxforge_call`'s own arguments.
fn call_arguments(arguments: &Map<String, Value>) -> Result<CallArguments<'_>, ApiFailure> {
    only(arguments, CALL, &["method", "params", EXPECTED_REVISION])?;
    let method = text(arguments, "method")?.ok_or_else(|| {
        validation(
            "luxforge_call needs method, a method's name",
            Some("method"),
        )
    })?;
    let params = match arguments.get("params") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(params)) => params.clone(),
        Some(_) => {
            return Err(validation("params is a JSON object", Some("params")));
        }
    };
    if params.contains_key(EXPECTED_REVISION) {
        return Err(validation(
            "give expected_revision as luxforge_call's own argument, beside params",
            Some("params.expected_revision"),
        ));
    }
    Ok(CallArguments {
        method,
        params,
        expected_revision: arguments.get(EXPECTED_REVISION),
    })
}

fn call_tool(
    owner: &mut Owner,
    client: &ClientInfo,
    arguments: &Map<String, Value>,
    cancel: &Cancel,
    images: bool,
) -> Result<ToolOutcome, Cancelled> {
    let mut warnings = Vec::new();
    let outcome = call(owner, client, arguments, cancel, images, &mut warnings)?;
    Ok(match outcome {
        Ok((value, pictures)) => ToolOutcome {
            value,
            is_error: false,
            images: pictures,
            warnings,
        },
        Err(error) => ToolOutcome::failed(error, warnings),
    })
}

/// Call a method as `luxforge_call` does: checked, its envelope built, sent once, and its
/// pictures read. Warnings for the result are added to `warnings`.
pub fn call(
    owner: &mut Owner,
    client: &ClientInfo,
    arguments: &Map<String, Value>,
    cancel: &Cancel,
    images: bool,
    warnings: &mut Vec<ApiFailure>,
) -> Result<Result<(Value, Vec<String>), ApiFailure>, Cancelled> {
    let held = |cancel: &Cancel| {
        if cancel.is_cancelled() {
            Err(Cancelled)
        } else {
            Ok(())
        }
    };
    let CallArguments {
        method,
        params,
        expected_revision,
    } = match call_arguments(arguments) {
        Ok(read) => read,
        Err(error) => return Ok(Err(error)),
    };
    let session = match owner.session(&client.label()) {
        Ok((session, warning)) => {
            warnings.extend(warning);
            session
        }
        Err(error) => return Ok(Err(error)),
    };
    held(cancel)?;
    let input = match &session.inputs {
        Ok(inputs) => inputs.get(method),
        Err(error) => {
            return Ok(Err(failure(
                PROTOCOL_ERROR,
                format!("this adapter cannot offer the live registry: {error}"),
                None,
            )));
        }
    };
    let Some(input) = input.cloned() else {
        return Ok(Err(validation(
            format!("unknown method {method}; luxforge_methods lists them"),
            Some("method"),
        )));
    };
    let entry = session.entry(method).cloned().unwrap_or(Value::Null);
    if !callable(&entry) {
        return Ok(Err(failure(
            "forbidden",
            format!(
                "{method} needs permission authority, which only the Luxforge desktop has; ask \
                 the person to do it in Luxforge"
            ),
            Some(json!({"method": method})),
        )));
    }
    let mut checked_arguments = params;
    if let Some(revision) = expected_revision {
        checked_arguments.insert(EXPECTED_REVISION.into(), revision.clone());
    }
    let checked = match input.check(&Value::Object(checked_arguments)) {
        Ok(checked) => checked,
        Err(error) => {
            let parameter = match error.parameter.as_deref() {
                Some(EXPECTED_REVISION) => Some(EXPECTED_REVISION.to_owned()),
                Some(parameter) => Some(format!("params.{parameter}")),
                None => Some("params".to_owned()),
            };
            return Ok(Err(validation(error.message, parameter.as_deref())));
        }
    };
    let mut params = checked.params;
    let actor = client.actor();
    let revision = match (input.envelope(), input.expected_revision()) {
        (Envelope::Revision, ExpectedRevision::Optional) => match checked.expected_revision {
            Some(revision) => Some(revision),
            None => {
                let asset = params.get("asset_id").cloned().unwrap_or(Value::Null);
                let state = match owner.call("asset.state", json!({"asset_id": asset})) {
                    Ok(state) => state,
                    Err(error) => return Ok(Err(error)),
                };
                held(cancel)?;
                match state["revision"].as_u64() {
                    Some(revision) => Some(revision),
                    None => {
                        return Ok(Err(failure(
                            PROTOCOL_ERROR,
                            "asset.state answered no revision",
                            None,
                        )));
                    }
                }
            }
        },
        (_, _) => checked.expected_revision,
    };
    let request_id = match input.envelope() {
        Envelope::None => None,
        Envelope::Request | Envelope::Revision => {
            let (built, request_id) = envelope(revision, &actor);
            params.insert("mutation".into(), built);
            Some(request_id)
        }
    };
    let result = match owner.call(method, Value::Object(params)) {
        Ok(result) => result,
        Err(mut error) => {
            if error.code == CONNECTION_LOST
                && let Some(request_id) = request_id
            {
                error.code = OUTCOME_UNKNOWN.into();
                error.message.push_str(
                    "; the change may or may not have been applied, and it was not sent again: \
                     read the state before deciding what to do",
                );
                error.data = Some(json!({"method": method, "request_id": request_id}));
            }
            return Ok(Err(error));
        }
    };
    if let Some(job_id) = result["job_id"].as_str() {
        held(cancel)?;
        warnings.extend(released(owner, job_id));
    }
    let mut pictures = Vec::new();
    if images {
        for (field, path) in images::marked(&entry, &result) {
            match images::read(&field, &path) {
                Ok(picture) => pictures.push(picture),
                Err(warning) => warnings.push(warning),
            }
        }
    }
    Ok(Ok((result, pictures)))
}

/// The warning for a job a call started when `job.read` says it belongs to the clients that
/// requested it: it stops, if still running, once this adapter's connection closes, and no later
/// client can read it. Nothing when it belongs to the catalog, or cannot be read.
fn released(owner: &mut Owner, job_id: &str) -> Option<ApiFailure> {
    let record = owner.call("job.read", json!({"job_id": job_id})).ok()?;
    if record["ownership"] != "clients" {
        return None;
    }
    Some(ApiFailure {
        code: JOB_RELEASED.into(),
        message: format!(
            "job {job_id} belongs to the clients that requested it: if it is still running when \
             this adapter's connection to Luxforge ends (the MCP client stops luxforge-mcp, or \
             Luxforge quits), it stops, and no later client can read it; follow it to its end \
             with job.wait"
        ),
        job_id: Some(job_id.into()),
        data: Some(json!({
            "kind": record["kind"],
            "status": record["status"],
            "ownership": record["ownership"],
        })),
    })
}

#[cfg(test)]
mod tests;
