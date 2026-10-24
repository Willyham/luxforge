//! Framing, the two eras' version rules and the client's identity.
use super::*;

fn invalid_code(line: &str) -> (Option<Value>, i64) {
    match read(line.as_bytes()) {
        Incoming::Invalid { id, error } => (id, error.code),
        other => panic!("{line} read as {other:?}"),
    }
}

#[test]
fn a_line_reads_as_a_request_notification_or_ignored_response() {
    assert_eq!(
        read(br#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#),
        Incoming::Request(Request {
            id: json!(7),
            method: "tools/list".into(),
            params: json!({}),
        })
    );
    assert_eq!(
        read(br#"{"jsonrpc":"2.0","id":"a","method":"ping","params":{"x":1}}"#),
        Incoming::Request(Request {
            id: json!("a"),
            method: "ping".into(),
            params: json!({"x": 1}),
        })
    );
    assert_eq!(
        read(br#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":7}}"#),
        Incoming::Notification {
            method: "notifications/cancelled".into(),
            params: json!({"requestId": 7}),
        }
    );
    assert_eq!(
        read(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#),
        Incoming::Response
    );
}

#[test]
fn a_line_that_is_not_a_request_this_server_reads_is_refused() {
    assert_eq!(invalid_code("{not json"), (None, PARSE_ERROR));
    assert_eq!(
        invalid_code(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#),
        (None, INVALID_REQUEST),
        "batches were removed from MCP"
    );
    assert_eq!(
        invalid_code(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#),
        (None, INVALID_REQUEST),
        "an id is never null"
    );
    assert_eq!(
        invalid_code(r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#),
        (None, INVALID_REQUEST)
    );
    assert_eq!(
        invalid_code(r#"{"id":3,"method":"ping"}"#),
        (Some(json!(3)), INVALID_REQUEST)
    );
    assert_eq!(
        invalid_code(r#"{"jsonrpc":"2.0","id":3,"method":"ping","params":[1]}"#),
        (Some(json!(3)), INVALID_REQUEST)
    );
    assert_eq!(
        invalid_code(r#"{"jsonrpc":"2.0","id":3}"#),
        (Some(json!(3)), INVALID_REQUEST)
    );
}

#[test]
fn request_keys_tell_a_string_identity_from_a_number() {
    let request = |id: Value| Request {
        id,
        method: "ping".into(),
        params: json!({}),
    };
    assert_ne!(request(json!(1)).key(), request(json!("1")).key());
    assert_eq!(request(json!(1)).key(), json!(1).to_string());
}

fn meta(version: Value, capabilities: Option<Value>, client: Option<Value>) -> Value {
    let mut meta = json!({"io.modelcontextprotocol/protocolVersion": version});
    if let Some(capabilities) = capabilities {
        meta["io.modelcontextprotocol/clientCapabilities"] = capabilities;
    }
    if let Some(client) = client {
        meta["io.modelcontextprotocol/clientInfo"] = client;
    }
    json!({"_meta": meta})
}

#[test]
fn a_modern_request_names_this_servers_version_and_its_capabilities() {
    assert_eq!(modern(&json!({})), None, "no version: not a modern request");
    let context = modern(&meta(
        json!(MODERN_VERSION),
        Some(json!({})),
        Some(json!({"name": "claude-code", "title": "Claude Code", "version": "2"})),
    ))
    .unwrap()
    .unwrap();
    assert_eq!(context.era, Era::Modern);
    assert_eq!(context.client.title.as_deref(), Some("Claude Code"));
    let refused = modern(&meta(json!("1900-01-01"), Some(json!({})), None))
        .unwrap()
        .unwrap_err();
    assert_eq!(refused.code, UNSUPPORTED_PROTOCOL_VERSION);
    assert_eq!(
        refused.data,
        Some(json!({"supported": SUPPORTED_VERSIONS, "requested": "1900-01-01"}))
    );
    // A legacy revision is spoken only after initialize, never as a per-request version.
    let legacy = modern(&meta(json!("2025-11-25"), Some(json!({})), None))
        .unwrap()
        .unwrap_err();
    assert_eq!(legacy.code, UNSUPPORTED_PROTOCOL_VERSION);
    for malformed in [
        meta(json!(MODERN_VERSION), None, None),
        meta(json!(MODERN_VERSION), Some(json!([])), None),
        meta(json!(20260728), Some(json!({})), None),
    ] {
        let refused = modern(&malformed).unwrap().unwrap_err();
        assert_eq!(refused.code, INVALID_PARAMS, "{malformed}");
    }
}

#[test]
fn initialize_negotiates_the_clients_legacy_revision_or_the_newest() {
    assert_eq!(negotiate("2025-06-18"), "2025-06-18");
    assert_eq!(negotiate("2025-11-25"), "2025-11-25");
    assert_eq!(negotiate("2024-11-05"), "2025-11-25");
    assert_eq!(negotiate(MODERN_VERSION), "2025-11-25");
}

#[test]
fn the_label_and_actor_name_the_client_within_their_bounds() {
    let client = |value: Value| ClientInfo::read(&value);
    let named = client(json!({"name": "claude-code", "title": "Claude Code", "version": "1"}));
    assert_eq!(named.label(), "Claude Code via MCP");
    assert_eq!(named.actor(), "mcp:claude-code");
    let untitled = client(json!({"name": "codex", "version": "1"}));
    assert_eq!(untitled.label(), "codex via MCP");
    let nameless = client(json!(null));
    assert_eq!(nameless.label(), UNNAMED_CLIENT);
    assert_eq!(nameless.actor(), "mcp");
    let blank = client(json!({"name": " \u{7}\t", "version": "1"}));
    assert_eq!(
        (blank.label(), blank.actor()),
        (UNNAMED_CLIENT.into(), "mcp".into())
    );
    let long = client(json!({"name": "é".repeat(100), "title": "x".repeat(100), "version": "1"}));
    let label = long.label();
    assert_eq!(label.chars().count(), 64);
    assert!(label.ends_with(" via MCP"), "{label}");
    let actor = long.actor();
    assert!(actor.len() <= 128 && actor.len() > 120, "{}", actor.len());
    assert!(actor.starts_with("mcp:é"));
}

#[test]
fn a_modern_result_carries_its_type_and_the_servers_identity_and_a_legacy_one_does_not() {
    let mut body = Map::new();
    body.insert("tools".into(), json!([]));
    let modern = result(&json!(1), Era::Modern, body.clone());
    assert_eq!(modern["result"]["resultType"], "complete");
    assert_eq!(
        modern["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        SERVER_NAME
    );
    let legacy = result(&json!(1), Era::Legacy, body);
    assert_eq!(
        legacy,
        json!({"jsonrpc": "2.0", "id": 1, "result": {"tools": []}})
    );
    assert_eq!(
        error(None, &RpcError::new(PARSE_ERROR, "bad")),
        json!({"jsonrpc": "2.0", "error": {"code": PARSE_ERROR, "message": "bad"}}),
        "an unreadable request is answered without an id, which is never null"
    );
}
