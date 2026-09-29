//! The preset methods across the `luxforge-json` process boundary: one Lightroom preset imported
//! from text sent over the client's pipe, listed, applied under the revision the client just
//! read, and a refused file answered as a structured error with nothing stored. What the presets
//! mean (mapping, reports, the stack an apply commits, capture, export, deduplication and the
//! event log) is proved in-process by the core's own tests; the process transport itself is
//! proved by `process`.

use luxforge_testbase::paths;
use luxforge_testkit::{JsonProcess, client::request_id};
use serde_json::{Value, json};

const ACTOR: &str = "presets-json-cli";

/// A fresh `{request_id, actor}` envelope, so every call is a new request.
fn request() -> Value {
    json!({"request_id": request_id("request"), "actor": ACTOR})
}

fn preset_file(name: &str) -> String {
    std::fs::read_to_string(paths::fixture(&format!("presets/{name}"))).expect("a preset fixture")
}

#[test]
fn a_preset_imports_lists_and_applies_and_a_refused_file_is_an_error_over_the_pipe() {
    let catalog = paths::temp_catalog("presets-json-cli");
    let mut client = JsonProcess::start(
        env!("CARGO_BIN_EXE_luxforge-json"),
        &["--catalog", catalog.to_str().expect("catalog is UTF-8")],
        "presets",
    );
    let job = client.call(
        "catalog.import",
        json!({
            "path": paths::jpeg().canonicalize().expect("the fixture"),
            "mutation": request(),
        }),
    )["job_id"]
        .clone();
    let status = client.settle("job.read", &job);
    assert_eq!(status["status"], "ready", "{status}");
    let asset = status["result"]["asset"]["id"].clone();

    // The file's text crosses the pipe as one JSON string, and the record comes back whole.
    let imported = client.call(
        "preset.import",
        json!({
            "content": preset_file("develop.xmp"),
            "mutation": request(),
            "file_name": "develop.xmp",
        }),
    );
    let record = &imported["preset"];
    assert_eq!(record["name"], json!("Soft Film"));
    assert_eq!(record["origin"]["kind"], json!("lightroom-xmp"));
    let listed = client.call("preset.list", json!({}))["presets"].clone();
    assert_eq!(listed.as_array().expect("presets").len(), 1);
    assert_eq!(listed[0]["id"], record["id"]);

    // Applying the listed settings under the revision the client read commits one labelled entry.
    let revision = client.call("asset.state", json!({"asset_id": asset}))["revision"].clone();
    let applied = client.call(
        "edit.apply-preset",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": revision, "request_id": request_id("apply"), "actor": ACTOR},
            "settings": listed[0]["settings"],
            "name": listed[0]["name"],
            "preset-id": listed[0]["id"],
        }),
    );
    assert_eq!(applied["outcome"], json!("applied"));
    let entries =
        client.call("history.list", json!({"asset_id": asset, "limit": 100}))["entries"].clone();
    assert_eq!(entries[0]["id"], applied["created_entry_id"]);
    assert_eq!(entries[0]["label"], json!("Preset: Soft Film"));

    // A refusal is the structured error the client reads, and stores nothing.
    let error = client.error(
        "preset.import",
        json!({
            "content": preset_file("profile.xmp"),
            "mutation": request(),
            "file_name": "profile.xmp",
        }),
    );
    assert_eq!(error["code"], json!("unsupported-input"), "{error}");
    assert_eq!(
        client.call("preset.list", json!({}))["presets"]
            .as_array()
            .expect("presets")
            .len(),
        1,
        "the refused file stored nothing"
    );
    client.finish();
    std::fs::remove_file(catalog).expect("the catalog is removed");
}
