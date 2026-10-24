//! `luxforge-mcp` driven over its standard streams, as an MCP client drives it, against a real
//! catalog owner serving a live session from this test process.
use crate::{
    live::{Live, Proxy, home_registry, registry_under},
    wire::{Era, Mcp, images, structured, warnings},
};
use luxforge_test_macros::slow;
use luxforge_testbase::paths;
use serde_json::{Value, json};
use std::{ffi::OsStr, path::Path};

/// A server attached to `live`'s catalog by `--catalog`.
fn attached(live: &Live) -> Mcp {
    Mcp::start(&[OsStr::new("--catalog"), live.catalog.as_os_str()], |_| {})
}

/// A server whose configuration is under `home`, so it finds running sessions in the registry
/// there.
fn at_home(home: &Path) -> Mcp {
    Mcp::start(&[], |command| {
        home_registry(command, home);
    })
}

fn tool_names(listed: &Value) -> Vec<&str> {
    listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect()
}

fn error_of(result: &Value) -> &Value {
    assert_eq!(result["isError"], true, "{result}");
    &structured(result)["error"]
}

#[test]
#[slow]
fn mcp_process_discovers_lists_and_refuses_without_a_session() {
    let home = paths::temp_dir("mcp-process-no-session");
    let mut mcp = at_home(&home);
    let discovered = mcp.ask(Era::Modern, "server/discover", json!({}));
    assert_eq!(
        discovered["result"]["supportedVersions"],
        json!(["2026-07-28", "2025-11-25", "2025-06-18"])
    );
    assert_eq!(discovered["result"]["capabilities"], json!({"tools": {}}));
    let listed = mcp.ask(Era::Modern, "tools/list", json!({}));
    assert_eq!(
        tool_names(&listed),
        ["luxforge_methods", "luxforge_describe", "luxforge_call"]
    );
    assert_eq!(listed["result"]["cacheScope"], "private");
    assert!(listed["result"]["ttlMs"].as_u64().unwrap() > 0);
    // A version this server does not speak, and a request naming none before initialize.
    let id = json!("old");
    mcp.send_request(
        Era::Modern,
        &id,
        "tools/list",
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2024-01-01",
                          "io.modelcontextprotocol/clientCapabilities": {}}}),
    );
    let refused = mcp.answer(&id);
    assert_eq!(refused["error"]["code"], -32022);
    assert_eq!(refused["error"]["data"]["requested"], "2024-01-01");
    let bare = json!("bare");
    mcp.send_request(Era::Modern, &bare, "tools/list", json!({}));
    assert_eq!(mcp.answer(&bare)["error"]["code"], -32602);
    // No Luxforge: the listing answers from the built-in registry, and a call says to open it.
    let methods = mcp.call(
        Era::Modern,
        "luxforge_methods",
        json!({"namespace": "edit"}),
    );
    assert_eq!(structured(&methods)["source"], "built-in");
    assert_eq!(structured(&methods)["session"]["code"], "no-session");
    assert!(structured(&methods)["count"].as_u64().unwrap() > 10);
    let described = mcp.call(
        Era::Modern,
        "luxforge_describe",
        json!({"methods": ["edit.set-basic"]}),
    );
    assert_eq!(
        structured(&described)["methods"][0]["expected_revision"],
        "optional"
    );
    let call = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "catalog.info"}),
    );
    let error = error_of(&call);
    assert_eq!(error["code"], "no-session", "{call}");
    assert!(error["message"].as_str().unwrap().contains("open Luxforge"));
    let unknown = mcp.ask(Era::Modern, "tools/call", json!({"name": "catalog_info"}));
    assert_eq!(unknown["error"]["code"], -32602);
    // A subscription is acknowledged with nothing honoured: the compact list never changes.
    let listen = mcp.request(
        Era::Modern,
        "subscriptions/listen",
        json!({"notifications": {"toolsListChanged": true}}),
    );
    let acknowledged = mcp.next_message();
    assert_eq!(
        acknowledged["params"],
        json!({"_meta": {"io.modelcontextprotocol/subscriptionId": listen}, "notifications": {}})
    );
    mcp.notify("notifications/cancelled", json!({"requestId": listen}));
    mcp.send_line("{not json");
    let unreadable = mcp.next_message();
    assert_eq!(unreadable["error"]["code"], -32700);
    assert!(unreadable.get("id").is_none());
    let (status, stderr) = mcp.finish();
    assert_eq!(status, Some(0), "{stderr}");

    // A legacy client: initialize negotiates its revision, then the same tools in that revision.
    let mut legacy = at_home(&home);
    let initialized = legacy.initialize("2024-11-05");
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(initialized["result"]["serverInfo"]["name"], "luxforge-mcp");
    assert_eq!(
        legacy.ask(Era::Legacy, "ping", json!({}))["result"],
        json!({})
    );
    let listed = legacy.ask(Era::Legacy, "tools/list", json!({}));
    assert_eq!(tool_names(&listed).len(), 3);
    let call = legacy.call(
        Era::Legacy,
        "luxforge_call",
        json!({"method": "catalog.info"}),
    );
    assert_eq!(error_of(&call)["code"], "no-session");
    let methods = legacy.call(Era::Legacy, "luxforge_methods", json!({"query": "undo"}));
    assert_eq!(structured(&methods)["source"], "built-in");
    assert_eq!(legacy.finish().0, Some(0));
    let mut negotiated = at_home(&home);
    assert_eq!(
        negotiated.initialize("2025-06-18")["result"]["protocolVersion"],
        "2025-06-18"
    );
    assert_eq!(negotiated.finish().0, Some(0));
    let _ = std::fs::remove_dir_all(home);
}

#[test]
#[slow]
fn mcp_process_never_guesses_between_sessions_and_attaches_once_one_is_running() {
    let Some(mut first) = Live::start("mcp-process-several", None) else {
        return;
    };
    let mut second = Live::start("mcp-process-several-other", None).unwrap();
    let home = first.scratch.join("home");
    let registry = registry_under(&home);
    let mut mcp = at_home(&home);
    // Started before Luxforge is open, it lists its tools and waits.
    assert_eq!(
        tool_names(&mcp.ask(Era::Modern, "tools/list", json!({}))).len(),
        3
    );
    for live in [&mut first, &mut second] {
        let catalog = live.catalog.clone();
        live.server
            .as_mut()
            .unwrap()
            .register(&registry, &catalog)
            .unwrap();
    }
    let call = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "catalog.info"}),
    );
    let error = error_of(&call);
    assert_eq!(error["code"], "usage", "{call}");
    assert_eq!(error["data"]["sessions"].as_array().unwrap().len(), 2);
    let methods = mcp.call(
        Era::Modern,
        "luxforge_methods",
        json!({"query": "catalog.info"}),
    );
    assert_eq!(structured(&methods)["session"]["code"], "usage");
    // One desktop quits: the next call attaches to the other.
    drop(second);
    let info = mcp.luxforge(Era::Modern, "catalog.info", json!({}));
    assert_eq!(
        structured(&info)["path"],
        json!(std::fs::canonicalize(&first.catalog).unwrap())
    );
    assert!(
        warnings(&info).is_empty(),
        "the first attachment replaces nothing"
    );
    assert_eq!(mcp.finish().0, Some(0));
}

#[test]
#[slow]
fn mcp_process_edits_land_in_history_under_the_agents_name_and_conflict_when_stale() {
    let Some(live) = Live::start("mcp-process-edit", None) else {
        return;
    };
    let asset = live.develop(true);
    let mut mcp = attached(&live);
    assert_eq!(
        tool_names(&mcp.ask(Era::Modern, "tools/list", json!({}))).len(),
        3
    );
    let methods = mcp.call(
        Era::Modern,
        "luxforge_methods",
        json!({"namespace": "history"}),
    );
    assert_eq!(structured(&methods)["source"], "live");
    let clients = live.clients();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0]["label"], "Claude Code via MCP");
    let state = mcp.luxforge(Era::Modern, "asset.state", json!({"asset_id": asset}));
    let before = structured(&state)["revision"].as_u64().unwrap();
    assert_eq!(before, live.revision(&asset));
    // Without expected_revision the adapter reads the asset's revision just before the edit.
    let edited = mcp.luxforge(
        Era::Modern,
        "edit.set-basic",
        json!({"asset_id": asset, "exposure": 0.5}),
    );
    assert_eq!(structured(&edited)["outcome"], "applied", "{edited}");
    let text: Value = serde_json::from_str(edited["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(&text, structured(&edited), "the same JSON as text");
    assert_eq!(live.revision(&asset), before + 1);
    assert_eq!(live.history(&asset)[0]["actor"], "mcp:claude-code");
    // A stale revision given explicitly is an honest conflict, never retried or rebased.
    let stale = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "edit.set-basic", "params": {"asset_id": asset, "contrast": 10},
               "expected_revision": before}),
    );
    assert_eq!(error_of(&stale)["code"], "conflict", "{stale}");
    assert_eq!(live.revision(&asset), before + 1);
    let invalid = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "edit.set-basic", "params": {"asset_id": asset, "exposure": "bright"}}),
    );
    assert_eq!(error_of(&invalid)["data"]["parameter"], "params.exposure");
    let forbidden = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "module.permission.grant",
               "params": {"module_id": "m", "capability": "download", "scope": {}}}),
    );
    assert_eq!(error_of(&forbidden)["code"], "forbidden");
    // A draft begun in one call is there in the next: one connection for the adapter's life.
    let begun = mcp.luxforge(
        Era::Modern,
        "draft.begin",
        json!({"asset_id": asset, "action": "set-basic"}),
    );
    let draft = structured(&begun)["draft_id"].as_str().unwrap().to_owned();
    mcp.luxforge(
        Era::Modern,
        "draft.set",
        json!({"draft_id": draft, "fields": {"exposure": 0.75}}),
    );
    let committed = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "draft.commit", "params": {"draft_id": draft},
               "expected_revision": before + 1}),
    );
    assert_eq!(committed["isError"], false, "{committed}");
    assert_eq!(live.revision(&asset), before + 2);
    assert_eq!(live.history(&asset)[0]["actor"], "mcp:claude-code");
    assert_eq!(mcp.finish().0, Some(0));

    // A legacy client edits the same way, its results in its own revision's shape.
    let mut legacy = attached(&live);
    legacy.initialize("2025-11-25");
    let edited = legacy.luxforge(
        Era::Legacy,
        "edit.set-basic",
        json!({"asset_id": asset, "exposure": -0.25}),
    );
    assert_eq!(structured(&edited)["outcome"], "applied");
    assert_eq!(live.revision(&asset), before + 3);
    assert_eq!(legacy.finish().0, Some(0));
}

/// `job.wait` on `job` after `change`, holding up to 30 s, as a request whose answer the test
/// reads later.
fn hold_job_wait(mcp: &mut Mcp, job: &str, change: &Value) -> Value {
    mcp.request(
        Era::Modern,
        "tools/call",
        json!({"name": "luxforge_call", "arguments": {"method": "job.wait",
               "params": {"job_id": job, "after": change, "timeout_ms": 30_000}}}),
    )
}

#[test]
#[slow]
fn mcp_process_follows_a_held_job_to_its_end_and_attaches_the_preview() {
    let Some(live) = Live::start("mcp-process-job", None) else {
        return;
    };
    let asset = live.develop(false);
    let mut mcp = attached(&live);
    // The preparation is held before its work, so the agent's wait is held with it.
    live.sources.shut();
    let started = mcp.luxforge(Era::Modern, "source.prepare", json!({"asset_id": asset}));
    let job = structured(&started)["job_id"].as_str().unwrap().to_owned();
    assert_eq!(
        warnings(&started),
        ["job-released"],
        "it belongs to the adapter's connection"
    );
    let mut observed = mcp.luxforge(
        Era::Modern,
        "job.wait",
        json!({"job_id": job, "timeout_ms": 0}),
    );
    let finished = loop {
        let status = structured(&observed)["job"]["status"].clone();
        if status != "queued" && status != "running" {
            break structured(&observed)["job"].clone();
        }
        let requests = live.requests();
        let id = hold_job_wait(&mut mcp, &job, &structured(&observed)["change"]);
        live.wait_for_requests(requests + 1);
        live.sources.open();
        observed = mcp.answer(&id)["result"].clone();
        assert_eq!(observed["isError"], false, "{observed}");
    };
    assert_eq!(finished["status"], "ready", "{finished}");

    // A photograph's preview: read, its render followed to its end, then read again cached. Each
    // answer that names the picture attaches it.
    let item = json!({"kind": "photo", "asset_id": asset});
    let first = mcp.luxforge(
        Era::Modern,
        "preview.read",
        json!({"item": item, "tier": "grid"}),
    );
    if let Some(render) = structured(&first)["job_id"].as_str() {
        let render = render.to_owned();
        let mut waited = mcp.luxforge(Era::Modern, "job.wait", json!({"job_id": render}));
        while matches!(
            structured(&waited)["job"]["status"].as_str(),
            Some("queued" | "running")
        ) {
            let change = structured(&waited)["change"].clone();
            waited = mcp.luxforge(
                Era::Modern,
                "job.wait",
                json!({"job_id": render, "after": change, "timeout_ms": 30_000}),
            );
        }
        assert_eq!(structured(&waited)["job"]["status"], "ready", "{waited}");
        assert_eq!(
            images(&waited).len(),
            1,
            "job.wait's result attaches the render"
        );
    }
    let cached = mcp.luxforge(
        Era::Modern,
        "preview.read",
        json!({"item": item, "tier": "grid"}),
    );
    let path = structured(&cached)["preview"]["path"].as_str().unwrap();
    let pictures = images(&cached);
    assert_eq!(pictures.len(), 1, "{cached}");
    assert!(pictures[0].starts_with(&[0xff, 0xd8]), "a JPEG");
    assert_eq!(pictures[0], std::fs::read(path).unwrap());
    assert_eq!(mcp.finish().0, Some(0));
    // --no-images keeps the path as data and attaches nothing.
    let mut plain = Mcp::start(
        &[
            OsStr::new("--catalog"),
            live.catalog.as_os_str(),
            OsStr::new("--no-images"),
        ],
        |_| {},
    );
    let read = plain.luxforge(
        Era::Modern,
        "preview.read",
        json!({"item": item, "tier": "grid"}),
    );
    assert!(images(&read).is_empty());
    assert!(structured(&read)["preview"]["path"].is_string());
    assert_eq!(plain.finish().0, Some(0));
}

#[test]
fn mcp_process_bounds_its_queue_and_drops_cancelled_requests() {
    let Some(live) = Live::start("mcp-process-queue", None) else {
        return;
    };
    let mut mcp = attached(&live);
    let now = mcp.luxforge(
        Era::Modern,
        "events.wait",
        json!({"after": 0, "timeout_ms": 0}),
    );
    let sequence = structured(&now)["current_sequence"].clone();
    // A held wait occupies the one call in flight until the log records an event.
    let requests = live.requests();
    let held = mcp.request(
        Era::Modern,
        "tools/call",
        json!({"name": "luxforge_call", "arguments": {"method": "events.wait",
               "params": {"after": sequence, "timeout_ms": 30_000}}}),
    );
    live.wait_for_requests(requests + 1);
    let queued: Vec<Value> = (0..32)
        .map(|_| mcp.request(Era::Modern, "tools/list", json!({})))
        .collect();
    let overflow = mcp.request(Era::Modern, "tools/list", json!({}));
    let refused = mcp.answer(&overflow);
    assert_eq!(refused["error"]["code"], -31000, "{refused}");
    assert_eq!(refused["error"]["data"]["limit"], 32);
    assert!(refused["error"]["message"].as_str().unwrap().contains("32"));
    // A queued request cancelled is never answered; the running one's answer is withheld. An
    // unreadable line's immediate answer proves the reader took each cancellation first.
    mcp.notify("notifications/cancelled", json!({"requestId": queued[4]}));
    mcp.notify(
        "notifications/cancelled",
        json!({"requestId": held, "reason": "the user moved on"}),
    );
    mcp.send_line("{not json");
    assert_eq!(mcp.next_message()["error"]["code"], -32700);
    // The person's own identification is an event, which ends the held wait.
    live.ask("session.identify", json!({"label": "Luxforge"}));
    for (index, id) in queued.iter().enumerate() {
        if index != 4 {
            let listed = mcp.answer(id);
            assert_eq!(listed["result"]["tools"].as_array().unwrap().len(), 3);
        }
    }
    // The adapter still serves the same connection, and the Luxforge job API was never touched.
    let after = mcp.luxforge(Era::Modern, "catalog.info", json!({}));
    assert!(warnings(&after).is_empty());
    assert_eq!(mcp.finish().0, Some(0));
}

#[test]
#[slow]
fn mcp_process_reports_outcome_unknown_on_a_lost_connection_and_attaches_again() {
    let real = paths::temp_path("mcp-real-session.json");
    let Some(live) = Live::start("mcp-process-lost", Some(&real)) else {
        return;
    };
    let asset = live.develop(true);
    let proxy = Proxy::start(&live, &real, Some("edit.set-basic"));
    let mut mcp = attached(&live);
    let before = live.revision(&asset);
    let lost = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "edit.set-basic", "params": {"asset_id": asset, "exposure": 0.5},
               "expected_revision": before}),
    );
    let error = error_of(&lost);
    assert_eq!(error["code"], "outcome-unknown", "{lost}");
    assert_eq!(error["data"]["method"], "edit.set-basic");
    assert!(error["data"]["request_id"].is_string());
    // It was applied: the answer was what was lost.
    assert_eq!(live.revision(&asset), before + 1);
    assert_eq!(live.history(&asset)[0]["actor"], "mcp:claude-code");
    let again = mcp.luxforge(Era::Modern, "catalog.info", json!({}));
    assert_eq!(warnings(&again), ["session-replaced"]);
    assert!(warnings(&mcp.luxforge(Era::Modern, "catalog.info", json!({}))).is_empty());
    let methods: Vec<String> = proxy.methods.try_iter().collect();
    assert_eq!(
        methods,
        [
            "schema.list",
            "session.identify",
            "edit.set-basic",
            "schema.list",
            "session.identify",
            "catalog.info",
            "catalog.info",
        ],
        "the edit was sent once and never retried"
    );
    assert_eq!(mcp.finish().0, Some(0));
    drop(proxy);
    let _ = std::fs::remove_file(real);
}
