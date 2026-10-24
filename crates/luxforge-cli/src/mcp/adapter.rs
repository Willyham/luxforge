//! The worker's side of the server: each request answered in the era it arrives in. Discovery,
//! the legacy handshake, `ping`, subscriptions and the tool list are answered here; a tool call is
//! [`Tools::call`]'s, through the adapter's one [`Owner`].
use super::{
    changes::Changes,
    images::MIME_TYPE,
    owner::Owner,
    protocol::{
        self, ClientInfo, Context, Era, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND,
        MODERN_VERSION, Request, RpcError, SUPPORTED_VERSIONS,
    },
    serve::{Cancel, Handle},
    tools::{Profile, ToolOutcome, Tools},
};
use serde_json::{Map, Value, json};

/// How long a client may consider `server/discover` and the compact `tools/list` fresh. The
/// compact list is fixed for the life of the process, and the version and capabilities with it.
pub const TTL_MS: u64 = 3_600_000;

/// How long a client may consider a typed `tools/list` fresh: the list follows the registry of the
/// session attached, so a client that does not subscribe to its changes asks again soon.
pub const TYPED_TTL_MS: u64 = 60_000;

/// What the server tells the model about itself, with the compact tools.
const INSTRUCTIONS: &str = "Luxforge is a non-destructive photo editor. These tools operate a \
    Luxforge catalog (the one the running Luxforge desktop has open, or the one this server was \
    started headless over) through the same operations a person uses: find methods with \
    luxforge_methods, read their parameters with luxforge_describe, and call them with \
    luxforge_call. Every edit lands in history under this agent's name and can be undone; \
    original files are never modified, and an agent can never grant consent or permissions.";

/// What the server tells the model about itself, with one tool per method.
const TYPED_INSTRUCTIONS: &str = "Luxforge is a non-destructive photo editor. These tools operate \
    a Luxforge catalog (the one the running Luxforge desktop has open, or the one this server \
    was started headless over) through the same operations a person uses: each tool is one \
    Luxforge method, named after it with each . replaced by _ (asset_state is asset.state). The \
    adapter builds every mutation envelope; a method that checks an asset's revision takes \
    expected_revision, read from asset.state when it is left out. Every edit lands in history \
    under this agent's name and can be undone; original files are never modified, and an agent \
    can never grant consent or permissions. Until this server reaches Luxforge the list is its \
    built-in registry, and it changes when Luxforge's registry does.";

/// The legacy session `initialize` selected: its negotiated revision and the client it named.
struct Legacy {
    client: ClientInfo,
}

/// The request handler the worker runs.
pub struct Adapter {
    tools: Tools,
    owner: Owner,
    legacy: Option<Legacy>,
    changes: Changes,
}

impl Adapter {
    /// The handler serving `tools` through `owner`, which reports its registries to `changes`.
    pub fn new(tools: Tools, mut owner: Owner, changes: Changes) -> Self {
        owner.report_to(changes.clone());
        Self {
            tools,
            owner,
            legacy: None,
            changes,
        }
    }

    fn instructions(&self) -> &'static str {
        match self.tools.profile {
            Profile::Compact => INSTRUCTIONS,
            Profile::Typed(_) => TYPED_INSTRUCTIONS,
        }
    }

    fn capabilities(&self) -> Value {
        if self.tools.profile.list_changed() {
            json!({"tools": {"listChanged": true}})
        } else {
            json!({"tools": {}})
        }
    }

    /// `initialize`: legacy semantics for the life of the process, at the client's revision when
    /// this server speaks it, otherwise the newest legacy one. Its answer is written here, before
    /// any list change it is then told of.
    fn initialize(&mut self, request: &Request) -> Vec<Value> {
        if self.legacy.is_some() {
            return vec![protocol::error(
                Some(&request.id),
                &RpcError::new(INVALID_REQUEST, "this server is already initialized"),
            )];
        }
        let Some(requested) = request.params["protocolVersion"].as_str() else {
            return vec![protocol::error(
                Some(&request.id),
                &RpcError::new(INVALID_PARAMS, "initialize names its protocolVersion"),
            )];
        };
        let version = protocol::negotiate(requested);
        self.legacy = Some(Legacy {
            client: ClientInfo::read(&request.params["clientInfo"]),
        });
        let mut result = Map::new();
        result.insert("protocolVersion".into(), json!(version));
        result.insert("capabilities".into(), self.capabilities());
        result.insert("serverInfo".into(), protocol::server_info());
        result.insert("instructions".into(), json!(self.instructions()));
        self.changes
            .initialized(&protocol::result(&request.id, Era::Legacy, result));
        Vec::new()
    }

    /// The era and client a request is answered for, or its refusal.
    fn context(&self, request: &Request) -> Result<Context, RpcError> {
        if let Some(modern) = protocol::modern(&request.params) {
            return modern;
        }
        match &self.legacy {
            Some(legacy) => Ok(Context {
                era: Era::Legacy,
                client: legacy.client.clone(),
            }),
            None => Err(RpcError::new(
                INVALID_PARAMS,
                format!(
                    "the request names no protocol version: a {MODERN_VERSION} request carries \
                     _meta io.modelcontextprotocol/protocolVersion and \
                     io.modelcontextprotocol/clientCapabilities, and an earlier client sends \
                     initialize first"
                ),
            )
            .with_data(json!({"supported": SUPPORTED_VERSIONS}))),
        }
    }

    fn discover(&self, request: &Request) -> Value {
        let mut result = Map::new();
        result.insert("supportedVersions".into(), json!(SUPPORTED_VERSIONS));
        result.insert("capabilities".into(), self.capabilities());
        result.insert("instructions".into(), json!(self.instructions()));
        result.insert("ttlMs".into(), json!(TTL_MS));
        result.insert("cacheScope".into(), json!("private"));
        protocol::result(&request.id, Era::Modern, result)
    }

    /// `subscriptions/listen`: acknowledged with the notifications this server honours, written
    /// here, and kept open, unanswered, until the client cancels it. A typed profile honours
    /// `toolsListChanged`; the compact list never changes, so it honours none.
    fn listen(&self, request: &Request) -> Vec<Value> {
        self.changes.listen(
            &request.id,
            request.params["notifications"]["toolsListChanged"] == json!(true),
        );
        Vec::new()
    }

    fn list(&mut self, request: &Request, context: &Context) -> Value {
        if request.params.get("cursor").is_some() {
            return protocol::error(
                Some(&request.id),
                &RpcError::new(INVALID_PARAMS, "invalid cursor: tools/list has one page"),
            );
        }
        let tools = match self.tools.list(&mut self.owner, &context.client) {
            Ok(tools) => tools,
            Err(error) => return protocol::error(Some(&request.id), &error),
        };
        self.changes.offer(&tools);
        let mut result = Map::new();
        result.insert("tools".into(), Value::Array(tools));
        if context.era == Era::Modern {
            let ttl = match self.tools.profile {
                Profile::Compact => TTL_MS,
                Profile::Typed(_) => TYPED_TTL_MS,
            };
            result.insert("ttlMs".into(), json!(ttl));
            result.insert("cacheScope".into(), json!("private"));
        }
        protocol::result(&request.id, context.era, result)
    }

    /// `tools/call`: no answer at all when the call stopped at a cancellation.
    fn call(&mut self, request: &Request, context: &Context, cancel: &Cancel) -> Option<Value> {
        let refuse = |message: &str| {
            Some(protocol::error(
                Some(&request.id),
                &RpcError::new(INVALID_PARAMS, message),
            ))
        };
        let Some(name) = request.params["name"].as_str() else {
            return refuse("tools/call names its tool");
        };
        let empty = Map::new();
        let arguments = match request.params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(arguments)) => arguments,
            Some(_) => return refuse("tools/call's arguments are an object"),
        };
        match self
            .tools
            .call(&mut self.owner, &context.client, name, arguments, cancel)
        {
            Err(error) => Some(protocol::error(Some(&request.id), &error)),
            Ok(Err(_cancelled)) => None,
            Ok(Ok(outcome)) => Some(protocol::result(
                &request.id,
                context.era,
                tool_result(context.era, outcome),
            )),
        }
    }
}

/// A tool's outcome as a `CallToolResult`: the result as `structuredContent` and as one text
/// block of the same JSON, then any pictures, then any warnings as one text block
/// `{"warnings": [...]}`. A legacy result carries `structuredContent` only when it is an object, as
/// those revisions require.
fn tool_result(era: Era, outcome: ToolOutcome) -> Map<String, Value> {
    let mut content = vec![json!({"type": "text", "text": outcome.value.to_string()})];
    content.extend(
        outcome
            .images
            .into_iter()
            .map(|data| json!({"type": "image", "data": data, "mimeType": MIME_TYPE})),
    );
    if !outcome.warnings.is_empty() {
        content.push(json!({
            "type": "text",
            "text": json!({"warnings": outcome.warnings}).to_string(),
        }));
    }
    let mut result = Map::new();
    result.insert("content".into(), Value::Array(content));
    if era == Era::Modern || outcome.value.is_object() {
        result.insert("structuredContent".into(), outcome.value);
    }
    result.insert("isError".into(), json!(outcome.is_error));
    result
}

impl Handle for Adapter {
    fn handle(&mut self, request: &Request, cancel: &Cancel) -> Vec<Value> {
        if request.method == "initialize" {
            return self.initialize(request);
        }
        let context = match self.context(request) {
            Ok(context) => context,
            Err(error) => return vec![protocol::error(Some(&request.id), &error)],
        };
        let answer = match (context.era, request.method.as_str()) {
            (Era::Modern, "server/discover") => self.discover(request),
            (Era::Modern, "subscriptions/listen") => return self.listen(request),
            (Era::Legacy, "ping") => protocol::result(&request.id, Era::Legacy, Map::new()),
            (_, "tools/list") => self.list(request, &context),
            (_, "tools/call") => match self.call(request, &context, cancel) {
                Some(answer) => answer,
                None => return Vec::new(),
            },
            (_, method) => protocol::error(
                Some(&request.id),
                &RpcError::new(METHOD_NOT_FOUND, format!("Method not found: {method}")),
            ),
        };
        vec![answer]
    }
}

#[cfg(test)]
mod tests;
