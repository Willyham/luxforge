//! The MCP wire ([protocol versions](../../../../docs/design/mcp.md#protocol-versions)): one
//! JSON-RPC 2.0 message per line, read into [`Incoming`], and the two eras a request is answered
//! in. A request whose `_meta` names `io.modelcontextprotocol/protocolVersion` is modern
//! (2026-07-28): served statelessly, its result carrying `resultType` and the server's identity. An
//! `initialize` request selects legacy semantics (2025-11-25 or 2025-06-18) for the process, and a
//! request without a modern version is then served as that revision defines it, with no
//! `resultType` and no `_meta`.
use serde_json::{Map, Value, json};

/// The modern revision this server speaks.
pub const MODERN_VERSION: &str = "2026-07-28";
/// The legacy revisions `initialize` negotiates, newest first: a client asking for another is
/// answered with the first.
pub const LEGACY_VERSIONS: [&str; 2] = ["2025-11-25", "2025-06-18"];
/// Every revision this server speaks, newest first, as `server/discover` and a version refusal
/// list them.
pub const SUPPORTED_VERSIONS: [&str; 3] = [MODERN_VERSION, LEGACY_VERSIONS[0], LEGACY_VERSIONS[1]];

/// The server's programmatic name.
pub const SERVER_NAME: &str = "luxforge-mcp";
/// The server's display name.
pub const SERVER_TITLE: &str = "Luxforge";

/// The JSON text could not be parsed.
pub const PARSE_ERROR: i64 = -32700;
/// The message is not a JSON-RPC request this server reads.
pub const INVALID_REQUEST: i64 = -32600;
/// No such method, or none in the era the request is served in.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The method's parameters are malformed, a modern request lacks its required `_meta` fields, or
/// a tool is unknown.
pub const INVALID_PARAMS: i64 = -32602;
/// The request's protocol version is not one this server speaks.
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
/// Every queue slot is taken: the request is refused unread. An application code, outside the
/// JSON-RPC reserved range as the 2026-07-28 error-code policy asks.
pub const QUEUE_FULL: i64 = -31000;

const PROTOCOL_VERSION: &str = "io.modelcontextprotocol/protocolVersion";
const CLIENT_CAPABILITIES: &str = "io.modelcontextprotocol/clientCapabilities";
const CLIENT_INFO: &str = "io.modelcontextprotocol/clientInfo";
const SERVER_INFO: &str = "io.modelcontextprotocol/serverInfo";

/// Which revision's rules a request is answered by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Era {
    /// 2026-07-28: version, capabilities and client identity on every request.
    Modern,
    /// 2025-11-25 or 2025-06-18, after `initialize`.
    Legacy,
}

/// A JSON-RPC error this server answers with.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

/// One request: its identity (a string or an integer), method and parameters (an object, empty
/// when the request carries none).
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: Value,
    pub method: String,
    pub params: Value,
}

impl Request {
    /// The identity as a key: its JSON text, so the string `"1"` and the number `1` differ.
    pub fn key(&self) -> String {
        self.id.to_string()
    }
}

/// One line read as a JSON-RPC message.
#[derive(Debug, PartialEq)]
pub enum Incoming {
    Request(Request),
    Notification {
        method: String,
        params: Value,
    },
    /// A response, which this server never asks for: ignored.
    Response,
    /// Not a message this server reads: answered with `error`, under the request's identity when
    /// it could be read.
    Invalid {
        id: Option<Value>,
        error: RpcError,
    },
}

/// Read one line as a JSON-RPC message.
pub fn read(line: &[u8]) -> Incoming {
    let invalid = |id: Option<Value>, message: &str| Incoming::Invalid {
        id,
        error: RpcError::new(INVALID_REQUEST, message),
    };
    let value: Value = match serde_json::from_slice(line) {
        Ok(value) => value,
        Err(error) => {
            return Incoming::Invalid {
                id: None,
                error: RpcError::new(PARSE_ERROR, format!("the line is not JSON: {error}")),
            };
        }
    };
    let Value::Object(mut message) = value else {
        return invalid(
            None,
            "a message is one JSON-RPC object; batches are not supported",
        );
    };
    let id = match message.remove("id") {
        None => None,
        Some(id @ Value::String(_)) => Some(id),
        Some(id @ Value::Number(_)) if id.is_i64() || id.is_u64() => Some(id),
        Some(_) => return invalid(None, "a request's id is a string or an integer"),
    };
    if message.get("jsonrpc") != Some(&json!("2.0")) {
        return invalid(id, "a message names jsonrpc \"2.0\"");
    }
    let method = match message.remove("method") {
        Some(Value::String(method)) => method,
        Some(_) => return invalid(id, "a message's method is a string"),
        None if message.contains_key("result") || message.contains_key("error") => {
            return Incoming::Response;
        }
        None => return invalid(id, "a request names its method"),
    };
    let params = match message.remove("params") {
        None | Some(Value::Null) => Value::Object(Map::new()),
        Some(params @ Value::Object(_)) => params,
        Some(_) => return invalid(id, "a message's params are an object"),
    };
    match id {
        Some(id) => Incoming::Request(Request { id, method, params }),
        None => Incoming::Notification { method, params },
    }
}

/// The client software a request names, as far as it names it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientInfo {
    pub name: Option<String>,
    pub title: Option<String>,
}

/// The longest label `session.identify` takes, in characters.
const LABEL_LIMIT: usize = 64;
/// The longest actor a mutation envelope takes, in bytes.
const ACTOR_LIMIT: usize = 128;
/// What the adapter calls itself to the desktop when the client gives no name.
pub const UNNAMED_CLIENT: &str = "MCP client";
const VIA: &str = " via MCP";

impl ClientInfo {
    /// Read an `Implementation` object; anything else names nothing.
    pub fn read(value: &Value) -> Self {
        let text = |field: &str| {
            value[field]
                .as_str()
                .map(|text| text.chars().filter(|c| !c.is_control()).collect::<String>())
                .map(|text| text.trim().to_owned())
                .filter(|text| !text.is_empty())
        };
        Self {
            name: text("name"),
            title: text("title"),
        }
    }

    /// The label the adapter identifies itself with: "*title or name* via MCP", the client's part
    /// shortened so the whole is at most 64 characters, or [`UNNAMED_CLIENT`].
    pub fn label(&self) -> String {
        match self.title.as_ref().or(self.name.as_ref()) {
            Some(client) => {
                let room = LABEL_LIMIT - VIA.chars().count();
                let client: String = client.chars().take(room).collect();
                format!("{}{VIA}", client.trim_end())
            }
            None => UNNAMED_CLIENT.to_owned(),
        }
    }

    /// The actor the adapter's mutations record: `mcp:<name>`, at most 128 bytes, or `mcp`.
    pub fn actor(&self) -> String {
        let Some(name) = &self.name else {
            return "mcp".into();
        };
        let mut actor = String::from("mcp:");
        for character in name.chars() {
            if actor.len() + character.len_utf8() > ACTOR_LIMIT {
                break;
            }
            actor.push(character);
        }
        actor
    }
}

/// The era-specific facts a request is answered with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub era: Era,
    pub client: ClientInfo,
}

/// A request's modern context: `None` when its `_meta` names no protocol version, so it is not a
/// modern request; a refusal when the version is not this server's or a required field is missing.
pub fn modern(params: &Value) -> Option<Result<Context, RpcError>> {
    let meta = &params["_meta"];
    let version = meta.get(PROTOCOL_VERSION)?;
    let Some(version) = version.as_str() else {
        return Some(Err(RpcError::new(
            INVALID_PARAMS,
            format!("_meta {PROTOCOL_VERSION} is a string"),
        )));
    };
    if version != MODERN_VERSION {
        return Some(Err(unsupported(version)));
    }
    if !meta[CLIENT_CAPABILITIES].is_object() {
        return Some(Err(RpcError::new(
            INVALID_PARAMS,
            format!("a {MODERN_VERSION} request carries _meta {CLIENT_CAPABILITIES}, an object"),
        )));
    }
    Some(Ok(Context {
        era: Era::Modern,
        client: ClientInfo::read(&meta[CLIENT_INFO]),
    }))
}

/// The refusal of a protocol version this server does not speak.
pub fn unsupported(requested: &str) -> RpcError {
    RpcError::new(
        UNSUPPORTED_PROTOCOL_VERSION,
        format!(
            "Unsupported protocol version {requested}; this server speaks {}",
            SUPPORTED_VERSIONS.join(", ")
        ),
    )
    .with_data(json!({"supported": SUPPORTED_VERSIONS, "requested": requested}))
}

/// The legacy revision `initialize` answers a client asking for `requested` with: the same one
/// when this server speaks it, otherwise the newest legacy revision.
pub fn negotiate(requested: &str) -> &'static str {
    LEGACY_VERSIONS
        .into_iter()
        .find(|version| *version == requested)
        .unwrap_or(LEGACY_VERSIONS[0])
}

/// The server's `Implementation`.
pub fn server_info() -> Value {
    json!({"name": SERVER_NAME, "title": SERVER_TITLE, "version": crate::VERSION})
}

/// A result response in `era`: a modern one carries `resultType` and the server's identity in its
/// `_meta`.
pub fn result(id: &Value, era: Era, mut result: Map<String, Value>) -> Value {
    if era == Era::Modern {
        result.insert("resultType".into(), json!("complete"));
        let meta = result
            .entry("_meta")
            .or_insert_with(|| Value::Object(Map::new()));
        meta[SERVER_INFO] = server_info();
    }
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

/// An error response, under the request's identity when it could be read.
pub fn error(id: Option<&Value>, error: &RpcError) -> Value {
    let mut body = json!({"code": error.code, "message": error.message});
    if let Some(data) = &error.data {
        body["data"] = data.clone();
    }
    let mut response = json!({"jsonrpc": "2.0", "error": body});
    if let Some(id) = id {
        response["id"] = id.clone();
    }
    response
}

/// A notification from the server.
pub fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

#[cfg(test)]
mod tests;
