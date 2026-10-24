//! The worker's side of the server: each request answered in the era it arrives in. Discovery,
//! the legacy handshake, `ping`, subscriptions and the tool list are answered here; a tool call is
//! [`Tools::call`]'s, through the adapter's one [`Owner`].
use super::{
    images::MIME_TYPE,
    owner::Owner,
    protocol::{
        self, ClientInfo, Context, Era, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND,
        MODERN_VERSION, Request, RpcError, SUPPORTED_VERSIONS,
    },
    serve::{Cancel, Handle, Subscriptions},
    tools::{ToolOutcome, Tools},
};
use serde_json::{Map, Value, json};

/// How long a client may consider `server/discover` and `tools/list` fresh. The compact list is
/// fixed for the life of the process, and the version and capabilities with it.
pub const TTL_MS: u64 = 3_600_000;

/// What the server tells the model about itself.
const INSTRUCTIONS: &str = "Luxforge is a non-destructive photo editor. These tools operate the \
    catalog the running Luxforge desktop has open, through the same operations a person uses: \
    find methods with luxforge_methods, read their parameters with luxforge_describe, and call \
    them with luxforge_call. Every edit lands in history under this agent's name and can be \
    undone; original files are never modified, and an agent can never grant consent or \
    permissions.";

/// The legacy session `initialize` selected: its negotiated revision and the client it named.
struct Legacy {
    client: ClientInfo,
}

/// The request handler the worker runs.
pub struct Adapter {
    tools: Tools,
    owner: Owner,
    legacy: Option<Legacy>,
    subscriptions: Subscriptions,
}

impl Adapter {
    pub fn new(tools: Tools, owner: Owner, subscriptions: Subscriptions) -> Self {
        Self {
            tools,
            owner,
            legacy: None,
            subscriptions,
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
    /// this server speaks it, otherwise the newest legacy one.
    fn initialize(&mut self, request: &Request) -> Value {
        if self.legacy.is_some() {
            return protocol::error(
                Some(&request.id),
                &RpcError::new(INVALID_REQUEST, "this server is already initialized"),
            );
        }
        let Some(requested) = request.params["protocolVersion"].as_str() else {
            return protocol::error(
                Some(&request.id),
                &RpcError::new(INVALID_PARAMS, "initialize names its protocolVersion"),
            );
        };
        let version = protocol::negotiate(requested);
        self.legacy = Some(Legacy {
            client: ClientInfo::read(&request.params["clientInfo"]),
        });
        let mut result = Map::new();
        result.insert("protocolVersion".into(), json!(version));
        result.insert("capabilities".into(), self.capabilities());
        result.insert("serverInfo".into(), protocol::server_info());
        result.insert("instructions".into(), json!(INSTRUCTIONS));
        protocol::result(&request.id, Era::Legacy, result)
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
        result.insert("instructions".into(), json!(INSTRUCTIONS));
        result.insert("ttlMs".into(), json!(TTL_MS));
        result.insert("cacheScope".into(), json!("private"));
        protocol::result(&request.id, Era::Modern, result)
    }

    /// `subscriptions/listen`: acknowledged with the notifications this server honours and kept
    /// open until the client cancels it. The compact list never changes, so it honours none.
    fn listen(&self, request: &Request) -> Vec<Value> {
        let honoured = if self.tools.profile.list_changed()
            && request.params["notifications"]["toolsListChanged"] == json!(true)
        {
            json!({"toolsListChanged": true})
        } else {
            json!({})
        };
        self.subscriptions.open(request.key());
        vec![protocol::notification(
            "notifications/subscriptions/acknowledged",
            json!({
                "_meta": {"io.modelcontextprotocol/subscriptionId": request.id},
                "notifications": honoured,
            }),
        )]
    }

    fn list(&self, request: &Request, era: Era) -> Value {
        if request.params.get("cursor").is_some() {
            return protocol::error(
                Some(&request.id),
                &RpcError::new(INVALID_PARAMS, "invalid cursor: tools/list has one page"),
            );
        }
        let mut result = Map::new();
        result.insert("tools".into(), Value::Array(self.tools.profile.tools()));
        if era == Era::Modern {
            result.insert("ttlMs".into(), json!(TTL_MS));
            result.insert("cacheScope".into(), json!("private"));
        }
        protocol::result(&request.id, era, result)
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
            return vec![self.initialize(request)];
        }
        let context = match self.context(request) {
            Ok(context) => context,
            Err(error) => return vec![protocol::error(Some(&request.id), &error)],
        };
        let answer = match (context.era, request.method.as_str()) {
            (Era::Modern, "server/discover") => self.discover(request),
            (Era::Modern, "subscriptions/listen") => return self.listen(request),
            (Era::Legacy, "ping") => protocol::result(&request.id, Era::Legacy, Map::new()),
            (_, "tools/list") => self.list(request, context.era),
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
