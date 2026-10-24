//! Each era's answers from the worker's handler, with no owner to attach to.
use super::*;
use crate::{
    live::failure,
    mcp::{
        owner::{Attached, Target},
        protocol::{INVALID_REQUEST, UNSUPPORTED_PROTOCOL_VERSION},
        serve::{Output, Subscriptions},
        tools::{Namespaces, Profile},
    },
};
use luxforge_core::ApiFailure;
use std::{
    io::Write,
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, Sender, channel},
    },
};

/// What the adapter writes itself, rather than answering through the worker.
struct Lines(Arc<Mutex<Sender<Value>>>);

impl Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let _ = self
                .0
                .lock()
                .unwrap()
                .send(serde_json::from_slice(line).unwrap());
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// No Luxforge is running.
struct Closed;

impl Target for Closed {
    fn attach(&mut self, _label: &str) -> Result<Attached, ApiFailure> {
        Err(failure("no-session", "Luxforge is not running", None))
    }
}

/// An adapter offering `profile` with no Luxforge running, and what it writes itself.
fn adapter_with(profile: Profile) -> (Adapter, Receiver<Value>) {
    let (written, output) = channel();
    let namespaces = match &profile {
        Profile::Compact => None,
        Profile::Typed(namespaces) => Some(namespaces.clone()),
    };
    let changes = Changes::new(
        namespaces,
        Output::new(Box::new(Lines(Arc::new(Mutex::new(written))))),
        Subscriptions::default(),
    );
    let adapter = Adapter::new(
        Tools {
            profile,
            images: true,
        },
        Owner::new(Box::new(Closed)),
        changes,
    );
    (adapter, output)
}

fn adapter() -> Adapter {
    adapter_with(Profile::Compact).0
}

fn modern_params(extra: Value) -> Value {
    let mut params = json!({"_meta": {
        "io.modelcontextprotocol/protocolVersion": MODERN_VERSION,
        "io.modelcontextprotocol/clientCapabilities": {},
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "1"},
    }});
    if let Value::Object(extra) = extra {
        params.as_object_mut().unwrap().extend(extra);
    }
    params
}

fn ask(adapter: &mut Adapter, method: &str, params: Value) -> Value {
    let mut answers = adapter.handle(
        &Request {
            id: json!(1),
            method: method.into(),
            params,
        },
        &Cancel::default(),
    );
    assert_eq!(answers.len(), 1, "{answers:?}");
    answers.remove(0)
}

#[test]
fn discovery_lists_every_version_and_the_tools_capability() {
    let discovered = ask(&mut adapter(), "server/discover", modern_params(json!({})));
    let result = &discovered["result"];
    assert_eq!(result["supportedVersions"], json!(SUPPORTED_VERSIONS));
    assert_eq!(result["capabilities"], json!({"tools": {}}));
    assert_eq!(
        (
            &result["ttlMs"],
            &result["cacheScope"],
            &result["resultType"]
        ),
        (&json!(TTL_MS), &json!("private"), &json!("complete"))
    );
}

#[test]
fn a_request_without_a_version_before_initialize_is_refused_and_an_unknown_version_too() {
    let mut adapter = adapter();
    let bare = ask(&mut adapter, "tools/list", json!({}));
    assert_eq!(bare["error"]["code"], INVALID_PARAMS);
    let mut params = modern_params(json!({}));
    params["_meta"]["io.modelcontextprotocol/protocolVersion"] = json!("2099-01-01");
    let refused = ask(&mut adapter, "tools/list", params);
    assert_eq!(refused["error"]["code"], UNSUPPORTED_PROTOCOL_VERSION);
    assert_eq!(refused["error"]["data"]["requested"], "2099-01-01");
    assert_eq!(refused["id"], 1);
}

#[test]
fn initialize_selects_legacy_semantics_for_the_process() {
    let (mut adapter, written) = adapter_with(Profile::Compact);
    let answers = adapter.handle(
        &Request {
            id: json!(1),
            method: "initialize".into(),
            params: json!({"protocolVersion": "2025-06-18", "capabilities": {},
                           "clientInfo": {"name": "legacy", "version": "1"}}),
        },
        &Cancel::default(),
    );
    assert!(answers.is_empty(), "written before any list change");
    let initialized = written.recv().unwrap();
    assert_eq!(initialized["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(initialized["result"]["serverInfo"]["name"], "luxforge-mcp");
    assert!(initialized["result"].get("resultType").is_none());
    assert_eq!(
        ask(&mut adapter, "ping", json!({})),
        json!({"jsonrpc": "2.0", "id": 1, "result": {}})
    );
    let listed = ask(&mut adapter, "tools/list", json!({}));
    assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 3);
    assert!(listed["result"].get("ttlMs").is_none() && listed["result"].get("_meta").is_none());
    // A modern request is still served as one beside the legacy session.
    let modern = ask(&mut adapter, "tools/list", modern_params(json!({})));
    assert_eq!(modern["result"]["cacheScope"], "private");
    let again = ask(
        &mut adapter,
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {},
               "clientInfo": {"name": "legacy", "version": "1"}}),
    );
    assert_eq!(again["error"]["code"], INVALID_REQUEST);
    let unknown = ask(&mut adapter, "server/discover", json!({}));
    assert_eq!(
        unknown["error"]["code"], METHOD_NOT_FOUND,
        "not a legacy method"
    );
}

#[test]
fn ping_is_legacy_only_and_unknown_methods_and_cursors_are_refused() {
    let mut adapter = adapter();
    assert_eq!(
        ask(&mut adapter, "ping", modern_params(json!({})))["error"]["code"],
        METHOD_NOT_FOUND
    );
    assert_eq!(
        ask(&mut adapter, "resources/list", modern_params(json!({})))["error"]["code"],
        METHOD_NOT_FOUND
    );
    assert_eq!(
        ask(
            &mut adapter,
            "tools/list",
            modern_params(json!({"cursor": "2"}))
        )["error"]["code"],
        INVALID_PARAMS
    );
    assert_eq!(
        ask(
            &mut adapter,
            "tools/call",
            modern_params(json!({"name": "nope"}))
        )["error"]["code"],
        INVALID_PARAMS
    );
}

#[test]
fn a_subscription_is_acknowledged_with_what_the_profile_honours_and_held_open() {
    for (profile, honoured) in [
        (Profile::Compact, json!({})),
        (
            Profile::Typed(Namespaces::All),
            json!({"toolsListChanged": true}),
        ),
    ] {
        let (adapter, written) = adapter_with(profile);
        let request = Request {
            id: json!("listen"),
            method: "subscriptions/listen".into(),
            params: modern_params(json!({"notifications": {"toolsListChanged": true}})),
        };
        assert!(
            adapter.listen(&request).is_empty(),
            "no response: the stream stays open"
        );
        assert_eq!(
            written.recv().unwrap(),
            json!({"jsonrpc": "2.0", "method": "notifications/subscriptions/acknowledged",
                "params": {"_meta": {"io.modelcontextprotocol/subscriptionId": "listen"},
                           "notifications": honoured}}),
        );
        adapter.changes.close();
    }
}

#[test]
fn a_typed_profile_advertises_list_changes_and_lists_the_built_in_registry_without_a_session() {
    let (mut adapter, _) = adapter_with(Profile::Typed(Namespaces::Only(vec!["asset".into()])));
    let discovered = ask(&mut adapter, "server/discover", modern_params(json!({})));
    assert_eq!(
        discovered["result"]["capabilities"],
        json!({"tools": {"listChanged": true}})
    );
    let listed = ask(&mut adapter, "tools/list", modern_params(json!({})));
    let tools = listed["result"]["tools"].as_array().unwrap();
    assert!(!tools.is_empty());
    assert!(
        tools
            .iter()
            .all(|tool| tool["name"].as_str().unwrap().starts_with("asset_"))
    );
    assert_eq!(listed["result"]["ttlMs"], TYPED_TTL_MS);
    let call = ask(
        &mut adapter,
        "tools/call",
        modern_params(json!({"name": "asset_state", "arguments": {"asset_id": "a"}})),
    );
    assert_eq!(
        call["result"]["structuredContent"]["error"]["code"],
        "no-session"
    );
}

#[test]
fn a_tool_result_carries_its_json_twice_and_a_failure_is_a_tool_error() {
    let mut adapter = adapter();
    let failed = ask(
        &mut adapter,
        "tools/call",
        modern_params(json!({"name": "luxforge_call", "arguments": {"method": "catalog.info"}})),
    );
    let result = &failed["result"];
    assert_eq!(result["isError"], true);
    assert_eq!(result["structuredContent"]["error"]["code"], "no-session");
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, result["structuredContent"]);
    let outcome = ToolOutcome {
        value: json!([1, 2]),
        is_error: false,
        images: vec!["/9j/2Q==".into()],
        warnings: vec![failure("image-too-large", "big", None)],
    };
    let legacy = tool_result(Era::Legacy, outcome);
    assert!(
        legacy.get("structuredContent").is_none(),
        "an array is not legacy structured content"
    );
    assert_eq!(
        legacy["content"][1],
        json!({"type": "image", "data": "/9j/2Q==", "mimeType": "image/jpeg"})
    );
    let warnings: Value =
        serde_json::from_str(legacy["content"][2]["text"].as_str().unwrap()).unwrap();
    assert_eq!(warnings["warnings"][0]["code"], "image-too-large");
}
