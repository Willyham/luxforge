//! The typed profiles across the process boundary: one tool per method without a session and
//! with one, the namespace filter and its refusal at launch, typed reads and edits, and the list
//! change a flag announces in each era, learnt from the event log on a second connection.
use crate::{
    live::{HIDDEN, Live, Proxy, home_registry},
    wire::{Era, Mcp, structured},
};
use luxforge_core::{ModuleRegistry, schemas};
use luxforge_test_macros::slow;
use luxforge_testbase::paths;
use serde_json::{Value, json};
use std::{ffi::OsStr, sync::atomic::Ordering, time::Duration};

fn names(listed: &Value) -> Vec<String> {
    listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

/// How many methods of the built-in registry a live client may call.
fn callable_methods() -> usize {
    schemas(&ModuleRegistry::builtin())["methods"]
        .as_object()
        .unwrap()
        .values()
        .filter(|entry| entry["authority"] != "permissions")
        .count()
}

#[test]
#[slow]
fn mcp_typed_profiles_list_without_a_session_filter_by_namespace_and_refuse_an_unknown_one() {
    let home = paths::temp_dir("mcp-typed-no-session");
    let start = |tools: &str| {
        Mcp::start(&[OsStr::new("--tools"), OsStr::new(tools)], |command| {
            home_registry(command, &home);
        })
    };
    let mut all = start("all");
    let discovered = all.ask(Era::Modern, "server/discover", json!({}));
    assert_eq!(
        discovered["result"]["capabilities"],
        json!({"tools": {"listChanged": true}})
    );
    let listed = all.ask(Era::Modern, "tools/list", json!({}));
    let every = names(&listed);
    assert_eq!(
        every.len(),
        callable_methods(),
        "one tool per callable method"
    );
    assert!(every.iter().any(|name| name == "asset_state"));
    assert!(!every.iter().any(|name| name == "module_permission_grant"));
    let call = all.call(Era::Modern, "catalog_info", json!({}));
    assert_eq!(call["isError"], true);
    assert_eq!(structured(&call)["error"]["code"], "no-session");
    let unknown = all.ask(
        Era::Modern,
        "tools/call",
        json!({"name": "luxforge_call", "arguments": {}}),
    );
    assert_eq!(unknown["error"]["code"], -32602);
    assert_eq!(all.finish().0, Some(0));

    let mut some = start("asset,edit");
    let listed = some.ask(Era::Modern, "tools/list", json!({}));
    let named = names(&listed);
    assert!(named.len() > 10);
    assert!(
        named
            .iter()
            .all(|name| name.starts_with("asset_") || name.starts_with("edit_")),
        "{named:?}"
    );
    assert_eq!(some.finish().0, Some(0));

    let refused = start("asset,nope");
    let (status, stderr) = refused.finish();
    assert_eq!(status, Some(2));
    let error: Value = serde_json::from_str(stderr.trim()).unwrap();
    assert_eq!(error["error"]["code"], "usage");
    assert!(
        error["error"]["message"].as_str().unwrap().contains("nope"),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(home);
}

/// Wait until the proxy has forwarded `count` more `method` requests.
fn forwarded(proxy: &Proxy, method: &str, count: usize) {
    let mut seen = 0;
    while seen < count {
        let next = proxy
            .methods
            .recv_timeout(Duration::from_secs(120))
            .unwrap_or_else(|error| panic!("no {method} was forwarded: {error}"));
        if next == method {
            seen += 1;
        }
    }
}

/// A typed server attached through the proxy, listing its tools, and its watch holding the event
/// log: the cursor read and the held wait have both been forwarded.
fn listening(live: &Live, era: Era, proxy: &Proxy) -> (Mcp, Option<Value>) {
    let mut mcp = Mcp::start(
        &[
            OsStr::new("--catalog"),
            live.catalog.as_os_str(),
            OsStr::new("--tools"),
            OsStr::new("all"),
        ],
        |_| {},
    );
    let subscription = match era {
        Era::Modern => {
            let listen = mcp.request(
                Era::Modern,
                "subscriptions/listen",
                json!({"notifications": {"toolsListChanged": true}}),
            );
            let acknowledged = mcp.next_message();
            assert_eq!(
                acknowledged["params"]["notifications"],
                json!({"toolsListChanged": true})
            );
            Some(listen)
        }
        Era::Legacy => {
            let initialized = mcp.initialize("2025-11-25");
            assert_eq!(
                initialized["result"]["capabilities"],
                json!({"tools": {"listChanged": true}})
            );
            None
        }
    };
    let listed = mcp.ask(era, "tools/list", json!({}));
    assert!(
        !names(&listed)
            .iter()
            .any(|name| name.starts_with("history_")),
        "the hidden methods are not offered"
    );
    forwarded(proxy, "events.wait", 2);
    (mcp, subscription)
}

#[test]
#[slow]
fn mcp_typed_tools_read_and_edit_and_a_flag_change_announces_the_new_list_in_each_era() {
    let real = paths::temp_path("mcp-typed-real-session.json");
    let Some(live) = Live::start_with_preferences("mcp-typed-changes", Some(&real)) else {
        return;
    };
    let asset = live.develop(true);
    let proxy = Proxy::start(&live, &real, None);
    proxy.hiding.store(true, Ordering::SeqCst);

    let (mut mcp, subscription) = listening(&live, Era::Modern, &proxy);
    let mut labels: Vec<String> = live
        .clients()
        .iter()
        .map(|client| client["label"].as_str().unwrap().to_owned())
        .collect();
    labels.sort();
    assert_eq!(
        labels,
        ["Claude Code via MCP", "Claude Code via MCP (tool list)"],
        "the watch is named apart from the agent"
    );
    // A typed read and a typed edit, which lands in history under the agent's name.
    let state = mcp.call(Era::Modern, "asset_state", json!({"asset_id": asset}));
    let before = structured(&state)["revision"].as_u64().unwrap();
    let edited = mcp.call(
        Era::Modern,
        "edit_set-basic",
        json!({"asset_id": asset, "exposure": 0.5}),
    );
    assert_eq!(structured(&edited)["outcome"], "applied", "{edited}");
    assert_eq!(live.revision(&asset), before + 1);
    assert_eq!(live.history(&asset)[0]["actor"], "mcp:claude-code");
    // The person turns a flag on, and the registry now lists the hidden methods.
    proxy.hiding.store(false, Ordering::SeqCst);
    live.ask("flags.set", json!({"flag": "ai.analysis", "value": true}));
    assert_eq!(
        mcp.next_message(),
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed",
               "params": {"_meta": {"io.modelcontextprotocol/subscriptionId": subscription}}})
    );
    let listed = mcp.ask(Era::Modern, "tools/list", json!({}));
    assert!(names(&listed).iter().any(|name| name == "history_list"));
    let history = mcp.call(
        Era::Modern,
        "history_list",
        json!({"asset_id": asset, "limit": 5}),
    );
    assert_eq!(history["isError"], false, "{history}");
    assert_eq!(mcp.finish().0, Some(0));

    // A legacy client hears of the same change untagged.
    proxy.hiding.store(true, Ordering::SeqCst);
    let _: Vec<String> = proxy.methods.try_iter().collect();
    let (mut legacy, _) = listening(&live, Era::Legacy, &proxy);
    proxy.hiding.store(false, Ordering::SeqCst);
    live.ask("flags.set", json!({"flag": "ai.analysis", "value": false}));
    assert_eq!(
        legacy.next_message(),
        json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"})
    );
    let listed = legacy.ask(Era::Legacy, "tools/list", json!({}));
    assert!(
        names(&listed)
            .iter()
            .any(|name| name.starts_with(&HIDDEN.replace('.', "_")))
    );
    assert_eq!(legacy.finish().0, Some(0));
    drop(proxy);
    let _ = std::fs::remove_file(real);
}
