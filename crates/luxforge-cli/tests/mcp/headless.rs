//! `luxforge-mcp --headless` driven over its standard streams, over its own `luxforge-json` child
//! (the one built beside it): an edit made and read back, no child left behind when the input
//! ends, a catalog a registered desktop session names refused before any child starts, and a
//! child that cannot open its catalog reported with what it wrote.
use crate::{
    live::{Live, home_registry, registry_under},
    wire::{Era, Mcp, structured, warnings},
};
use luxforge_core::OwnerHandle;
use luxforge_test_macros::slow;
use luxforge_testbase::paths;
use serde_json::{Value, json};
use std::{ffi::OsStr, path::Path};

/// A headless server over `catalog`, its configuration under `home`.
fn headless(catalog: &Path, home: &Path) -> Mcp {
    Mcp::start(
        &[
            OsStr::new("--headless"),
            OsStr::new("--catalog"),
            catalog.as_os_str(),
        ],
        |command| {
            home_registry(command, home);
        },
    )
}

fn error_of(result: &Value) -> &Value {
    assert_eq!(result["isError"], true, "{result}");
    &structured(result)["error"]
}

/// The diagnostic naming each `luxforge-json` child the server starts, up to its process.
const STARTED: &str = "luxforge-mcp: started luxforge-json (process ";

/// The processes of the `luxforge-json` children the server's diagnostics say it started.
fn started(stderr: &str) -> Vec<u32> {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix(STARTED))
        .map(|rest| rest.split(')').next().unwrap().parse().unwrap())
        .collect()
}

/// Follow a job the adapter started to its end, through the adapter.
fn finish_job(mcp: &mut Mcp, job_id: &Value) -> Value {
    loop {
        let waited = mcp.luxforge(
            Era::Modern,
            "job.wait",
            json!({"job_id": job_id, "timeout_ms": 30_000}),
        );
        let job = structured(&waited)["job"].clone();
        if !matches!(job["status"].as_str(), Some("queued" | "running")) {
            return job;
        }
    }
}

/// Whether no process holds `catalog` open: an owner in this process can open it.
fn catalog_free(catalog: &Path) -> bool {
    match OwnerHandle::start(catalog) {
        Ok((owner, join)) => {
            owner.stop();
            join.join().unwrap();
            true
        }
        Err(_) => false,
    }
}

#[test]
#[slow]
fn mcp_headless_edits_a_photo_reads_it_back_and_leaves_no_child_behind() {
    let scratch = paths::temp_dir("mcp-headless-edit");
    let catalog = scratch.join("catalog.sqlite");
    let original = scratch.join("original.jpg");
    std::fs::copy(paths::jpeg(), &original).unwrap();
    let mut mcp = headless(&catalog, &scratch.join("home"));
    // Discovery and the tool list answer before any owner starts.
    assert_eq!(
        mcp.ask(Era::Modern, "tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let started_job = mcp.luxforge(
        Era::Modern,
        "pick.develop",
        json!({"targets": {"kind": "paths", "paths": [original]}, "into": [],
               "confirm_removable": true}),
    );
    assert!(
        warnings(&started_job).is_empty(),
        "the first owner replaces nothing"
    );
    let developed = finish_job(&mut mcp, &structured(&started_job)["job_id"]);
    assert_eq!(developed["status"], "ready", "{developed}");
    let asset = developed["result"]["developed"][0]["asset_id"].clone();
    let prepared = mcp.luxforge(Era::Modern, "source.prepare", json!({"asset_id": asset}));
    let prepared = finish_job(&mut mcp, &structured(&prepared)["job_id"]);
    assert_eq!(prepared["status"], "ready", "{prepared}");
    let before = structured(&mcp.luxforge(Era::Modern, "asset.state", json!({"asset_id": asset})))
        ["revision"]
        .as_u64()
        .unwrap();
    let edited = mcp.luxforge(
        Era::Modern,
        "edit.set-basic",
        json!({"asset_id": asset, "exposure": 0.5}),
    );
    assert_eq!(structured(&edited)["outcome"], "applied", "{edited}");
    // Read back through the same owner: the revision moved and history names the agent.
    let state = mcp.luxforge(Era::Modern, "asset.state", json!({"asset_id": asset}));
    assert_eq!(structured(&state)["revision"], before + 1);
    let history = mcp.luxforge(
        Era::Modern,
        "history.list",
        json!({"asset_id": asset, "limit": 5}),
    );
    assert_eq!(
        structured(&history)["entries"][0]["actor"],
        "mcp:claude-code"
    );
    let info = mcp.luxforge(Era::Modern, "catalog.info", json!({}));
    assert_eq!(
        structured(&info)["path"],
        json!(std::fs::canonicalize(&catalog).unwrap())
    );
    let (status, stderr) = mcp.finish();
    assert_eq!(status, Some(0), "{stderr}");
    let children = started(&stderr);
    assert_eq!(children.len(), 1, "one child for the whole run: {stderr}");
    // The child ended by itself once its input closed, and the adapter waited for it.
    assert!(
        stderr.contains(&format!(
            "luxforge-mcp: luxforge-json (process {}) ended",
            children[0]
        )),
        "{stderr}"
    );
    assert!(!stderr.contains("did not end within"), "{stderr}");
    #[cfg(unix)]
    assert!(
        !std::process::Command::new("kill")
            .args(["-0", &children[0].to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success(),
        "the child process {} is gone",
        children[0]
    );
    assert!(catalog_free(&catalog), "no process holds the catalog");
    let _ = std::fs::remove_dir_all(scratch);
}

#[test]
#[slow]
#[cfg(unix)]
fn mcp_headless_starts_a_new_child_after_its_child_ends() {
    let scratch = paths::temp_dir("mcp-headless-replaced");
    let catalog = scratch.join("catalog.sqlite");
    let mut mcp = headless(&catalog, &scratch.join("home"));
    mcp.luxforge(Era::Modern, "catalog.info", json!({}));
    let child = started(&mcp.diagnostic(STARTED))[0];
    assert!(
        std::process::Command::new("kill")
            .args(["-9", &child.to_string()])
            .status()
            .unwrap()
            .success()
    );
    // The next call finds the child gone and starts another, warning that the old session's
    // state went with it; a call already on its way when the end is noticed reports the lost
    // connection instead, and the one after it attaches again.
    let mut result = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "catalog.info"}),
    );
    if result["isError"] == true {
        assert_eq!(error_of(&result)["code"], "connection-lost", "{result}");
        result = mcp.luxforge(Era::Modern, "catalog.info", json!({}));
    }
    assert_eq!(warnings(&result), ["session-replaced"], "{result}");
    let (status, stderr) = mcp.finish();
    assert_eq!(status, Some(0), "{stderr}");
    let children = started(&stderr);
    assert_eq!(children.len(), 2, "{stderr}");
    assert_eq!(children[0], child);
    assert!(catalog_free(&catalog));
    let _ = std::fs::remove_dir_all(scratch);
}

#[test]
#[slow]
fn mcp_headless_refuses_a_catalog_a_running_desktop_has_open_before_starting_a_child() {
    let Some(mut live) = Live::start("mcp-headless-registered", None) else {
        return;
    };
    let home = live.scratch.join("home");
    let registry = registry_under(&home);
    let catalog = live.catalog.clone();
    live.server
        .as_mut()
        .unwrap()
        .register(&registry, &catalog)
        .unwrap();
    let mut mcp = headless(&catalog, &home);
    let call = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "catalog.info"}),
    );
    let error = error_of(&call);
    assert_eq!(error["code"], "catalog-in-use", "{call}");
    assert_eq!(error["data"]["pid"], std::process::id());
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("without --headless")
    );
    let methods = mcp.call(Era::Modern, "luxforge_methods", json!({"query": "undo"}));
    assert_eq!(structured(&methods)["source"], "built-in");
    assert_eq!(structured(&methods)["session"]["code"], "catalog-in-use");
    let (status, stderr) = mcp.finish();
    assert_eq!(status, Some(0), "{stderr}");
    // Nothing was started: no child's diagnostics, and the desktop served no other client.
    assert!(started(&stderr).is_empty(), "{stderr}");
    assert!(!stderr.contains("luxforge-json:"), "{stderr}");
    assert!(live.clients().is_empty());
}

#[test]
#[slow]
fn mcp_headless_reports_a_child_that_cannot_open_its_catalog_with_what_it_wrote() {
    let scratch = paths::temp_dir("mcp-headless-locked");
    let catalog = scratch.join("catalog.sqlite");
    // Another owner, registered nowhere, holds the catalog's exclusive lock.
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let mut mcp = headless(&catalog, &scratch.join("home"));
    let call = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "catalog.info"}),
    );
    let error = error_of(&call).clone();
    assert_eq!(error["code"], "headless-failed", "{call}");
    let stderr = error["data"]["stderr"].as_str().unwrap();
    let written: Value = serde_json::from_str(stderr.lines().last().unwrap()).unwrap();
    assert!(written["error"]["code"].is_string(), "{stderr}");
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains(&format!("it wrote: {stderr}")),
        "{error}"
    );
    assert!(error["data"]["status"].is_string(), "{error}");
    assert_eq!(error["data"]["cause"]["code"], "connection-lost");
    // Not retried: the next call that needs the owner makes one new attempt of its own.
    let again = mcp.call(
        Era::Modern,
        "luxforge_call",
        json!({"method": "catalog.info"}),
    );
    assert_eq!(error_of(&again)["code"], "headless-failed");
    let (status, adapter_stderr) = mcp.finish();
    assert_eq!(status, Some(0), "{adapter_stderr}");
    assert_eq!(started(&adapter_stderr).len(), 2, "{adapter_stderr}");
    assert!(
        adapter_stderr.contains(&format!(
            "luxforge-json: {}",
            stderr.lines().last().unwrap()
        )),
        "the child's standard error is forwarded: {adapter_stderr}"
    );
    owner.stop();
    join.join().unwrap();
    let _ = std::fs::remove_dir_all(scratch);
}
