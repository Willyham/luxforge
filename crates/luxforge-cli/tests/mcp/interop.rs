//! `luxforge-mcp` under an independent client: `rmcp`, the official MCP Rust SDK, pinned as a
//! development dependency of this test alone ([verification](../../../../docs/design/mcp.md#verification)).
//! In each era (2026-07-28 through `server/discover`, 2025-11-25 through `initialize`) it starts
//! the real binary with the typed tools, lists them, calls a read and an edit, and receives the list
//! change a flag change announces: on its `subscriptions/listen` stream when modern, as a client
//! notification when legacy.
use crate::{
    live::{HIDDEN, Live, Proxy},
    wire::BINARY,
};
use luxforge_test_macros::slow;
use luxforge_testbase::paths;
use rmcp::{
    ClientHandler, ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::{
        CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
        ProtocolVersion, ServerNotification, SubscriptionFilter, Tool,
    },
    service::{NotificationContext, RunningService},
    transport::TokioChildProcess,
};
use serde_json::{Value, json};
use std::{sync::atomic::Ordering, time::Duration};
use tokio::{runtime::Runtime, sync::mpsc};

/// How long the test waits for what the server must send before it fails rather than hangs.
const STALL: Duration = Duration::from_secs(120);

/// The client: it names itself, and passes on each tool-list change a legacy server sends it.
struct Interop {
    version: ProtocolVersion,
    changed: mpsc::UnboundedSender<()>,
}

impl ClientHandler for Interop {
    async fn on_tool_list_changed(&self, _context: NotificationContext<RoleClient>) {
        let _ = self.changed.send(());
    }

    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("rmcp-interop", "1.0.0"),
        )
        .with_protocol_version(self.version.clone())
    }
}

type Client = RunningService<RoleClient, Interop>;

fn runtime() -> Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// Start `luxforge-mcp` with every typed tool on `live`'s catalog, and connect to it in the era
/// `version` selects.
fn connect(
    runtime: &Runtime,
    live: &Live,
    version: ProtocolVersion,
) -> (Client, mpsc::UnboundedReceiver<()>) {
    let (changed, changes) = mpsc::unbounded_channel();
    let mut command = tokio::process::Command::new(BINARY);
    command
        .arg("--catalog")
        .arg(&live.catalog)
        .args(["--tools", "all"]);
    let lifecycle = if version == ProtocolVersion::V_2026_07_28 {
        ClientLifecycleMode::Discover {
            preferred_versions: vec![version.clone()],
        }
    } else {
        ClientLifecycleMode::Initialize
    };
    let client = runtime
        .block_on(
            Interop { version, changed }
                .serve_with_lifecycle(TokioChildProcess::new(command).unwrap(), lifecycle),
        )
        .unwrap();
    (client, changes)
}

fn tool<'a>(tools: &'a [Tool], name: &str) -> Option<&'a Tool> {
    tools.iter().find(|tool| tool.name == name)
}

fn call(runtime: &Runtime, client: &Client, name: &'static str, arguments: Value) -> Value {
    let Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    let result: CallToolResult = runtime
        .block_on(client.call_tool(CallToolRequestParams::new(name).with_arguments(arguments)))
        .unwrap();
    assert_ne!(result.is_error, Some(true), "{name}: {result:?}");
    result.structured_content.unwrap()
}

/// Wait until the proxy has forwarded `count` more `method` requests.
fn forwarded(proxy: &Proxy, method: &str, count: usize) {
    let mut seen = 0;
    while seen < count {
        if proxy.methods.recv_timeout(STALL).unwrap() == method {
            seen += 1;
        }
    }
}

/// One era's journey: list, read, edit, then a flag change that adds the hidden methods, heard as
/// `heard` waits for it, and the new list.
fn journey(
    live: &Live,
    proxy: &Proxy,
    asset: &str,
    version: ProtocolVersion,
    flag: bool,
) -> Vec<String> {
    let runtime = runtime();
    // Timers and the child's pipes are created outside `block_on` too, so the whole journey runs in
    // the runtime's context.
    let _context = runtime.enter();
    proxy.hiding.store(true, Ordering::SeqCst);
    let _: Vec<String> = proxy.methods.try_iter().collect();
    let modern = version == ProtocolVersion::V_2026_07_28;
    let (client, mut legacy_changes) = connect(&runtime, live, version);
    let mut subscription = modern.then(|| {
        let subscription = runtime
            .block_on(client.listen(SubscriptionFilter::builder().tools_list_changed().build()))
            .unwrap();
        assert_eq!(subscription.acknowledged().tools_list_changed, Some(true));
        subscription
    });
    let tools = runtime.block_on(client.list_all_tools()).unwrap();
    assert!(tool(&tools, "history_list").is_none(), "hidden");
    let state_tool = tool(&tools, "asset_state").unwrap();
    assert_eq!(state_tool.title.as_deref(), Some("asset.state"));
    let annotations = state_tool.annotations.as_ref().unwrap();
    assert_eq!(
        (annotations.read_only_hint, annotations.destructive_hint),
        (Some(true), Some(false))
    );
    let before = call(&runtime, &client, "asset_state", json!({"asset_id": asset}))["revision"]
        .as_u64()
        .unwrap();
    let edited = call(
        &runtime,
        &client,
        "edit_set-basic",
        json!({"asset_id": asset, "exposure": if flag { 0.5 } else { -0.5 }}),
    );
    assert_eq!(edited["outcome"], "applied", "{edited}");
    assert_eq!(live.revision(asset), before + 1);
    assert_eq!(live.history(asset)[0]["actor"], "mcp:rmcp-interop");
    // The watch holds the event log: the cursor read and the held wait were both forwarded.
    forwarded(proxy, "events.wait", 2);
    proxy.hiding.store(false, Ordering::SeqCst);
    live.ask("flags.set", json!({"flag": "ai.analysis", "value": flag}));
    match &mut subscription {
        Some(subscription) => {
            let notification = runtime
                .block_on(tokio::time::timeout(STALL, subscription.next()))
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(
                matches!(
                    notification,
                    ServerNotification::ToolListChangedNotification(_)
                ),
                "{notification:?}"
            );
        }
        None => {
            runtime
                .block_on(tokio::time::timeout(STALL, legacy_changes.recv()))
                .unwrap()
                .unwrap();
        }
    }
    let tools = runtime.block_on(client.list_all_tools()).unwrap();
    if let Some(subscription) = &mut subscription {
        runtime.block_on(subscription.cancel()).unwrap();
    }
    runtime.block_on(client.cancel()).unwrap();
    tools
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect()
}

#[test]
#[slow]
fn mcp_interop_rmcp_lists_reads_edits_and_hears_a_list_change_in_each_era() {
    let real = paths::temp_path("mcp-interop-real-session.json");
    let Some(live) = Live::start_with_preferences("mcp-interop", Some(&real)) else {
        return;
    };
    let asset = live.develop(true);
    let proxy = Proxy::start(&live, &real, None);
    let hidden = HIDDEN.replace('.', "_");
    for (version, flag) in [
        (ProtocolVersion::V_2026_07_28, true),
        (ProtocolVersion::V_2025_11_25, false),
    ] {
        let listed = journey(&live, &proxy, &asset, version.clone(), flag);
        assert!(
            listed.iter().any(|name| name.starts_with(&hidden)),
            "{version:?}: the new list includes the methods the flag added"
        );
    }
    drop(proxy);
    let _ = std::fs::remove_file(real);
}
