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
    let fixture = paths::jpeg().canonicalize().expect("the fixture");
    let asset = client.open(&fixture, "presets-json-cli")["asset"]["id"].clone();

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
        "edit.apply-settings",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": revision, "request_id": request_id("apply"), "actor": ACTOR},
            "settings": listed[0]["settings"],
            "origin": {"kind": "preset", "name": listed[0]["name"], "preset_id": listed[0]["id"]},
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

/// What the desktop's create form and Copy chooser offer is a JSON client's too: `preset.groups`
/// lists the groups with their identities and defaults, describes them on a photo's entry, and
/// `preset.capture {groups}` reads the same settings as the fields those groups name.
#[test]
fn settings_groups_and_capture_by_groups_over_the_pipe() {
    let catalog = paths::temp_catalog("preset-groups-cli");
    let mut client = JsonProcess::start(
        env!("CARGO_BIN_EXE_luxforge-json"),
        &["--catalog", catalog.to_str().unwrap()],
        "preset-groups",
    );
    let asset = client.open(&paths::jpeg().canonicalize().unwrap(), ACTOR)["asset"]["id"].clone();
    let listed = client.call("preset.groups", json!({}));
    let ids: Vec<&str> = listed["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| group["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids[..3],
        [
            "luxforge.basic/white-balance",
            "luxforge.basic/tone",
            "luxforge.basic/colour"
        ]
    );
    let white = &listed["groups"][0];
    assert_eq!(white["title"], json!("Basic \u{00b7} White balance"));
    assert_eq!(white["default_checked"], json!(false));
    assert_eq!(white["per_photo"], json!(true));
    // A RAW development does not apply to a JPEG, so a white balance copied from a RAW photo is
    // skipped on one, and a JPEG's is skipped on a RAW photo.
    assert_eq!(white["kinds"][1]["kind"], json!("raw"));
    assert_eq!(white["kinds"][1]["captures"], json!({"set-raw": true}));
    assert_eq!(white["kinds"][1]["skipped"][0]["kind"], json!("jpeg"));
    assert_eq!(white["kinds"][1]["skipped"][0]["all"], json!(true));
    assert_eq!(white["kinds"][0]["skipped"][0]["kind"], json!("raw"));
    assert_eq!(listed["analysis"][0]["id"], json!("auto-tone"));
    assert_eq!(
        listed["analysis"][0]["overwrites"],
        json!(["luxforge.basic/tone", "luxforge.basic/colour"])
    );
    assert!(listed.get("photo").is_none());

    client.call("edit.set-basic", json!({"asset_id": asset, "exposure": 1, "mutation": {"expected_revision": 0, "request_id": request_id("edit"), "actor": ACTOR}}));
    let described = client.call("preset.groups", json!({"asset_id": asset}));
    assert_eq!(described["photo"]["kind"], json!("jpeg"));
    let states: Vec<(&str, &str)> = described["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|group| {
            (
                group["id"].as_str().unwrap(),
                group["state"].as_str().unwrap(),
            )
        })
        .collect();
    assert!(
        states.contains(&("luxforge.basic/tone", "custom")),
        "{states:?}"
    );
    assert_eq!(
        states
            .iter()
            .filter(|(_, state)| *state == "custom")
            .count(),
        1,
        "{states:?}"
    );

    let by_groups = client.call(
        "preset.capture",
        json!({"asset_id": asset, "groups": ["luxforge.basic/white-balance", "luxforge.mixer/hsl", "auto-tone"]}),
    );
    let by_fields = client.call(
        "preset.capture",
        json!({"asset_id": asset, "fields": {
            "set-basic": white["fields"]["set-basic"],
            "set-mixer": listed["groups"][7]["fields"]["set-mixer"],
            "auto-tone": true,
        }}),
    );
    assert_eq!(listed["groups"][7]["id"], json!("luxforge.mixer/hsl"));
    assert_eq!(by_groups, by_fields);
    assert_eq!(by_groups["settings"]["auto-tone"], json!({}));
    let by_groups = client.call(
        "preset.capture",
        json!({"asset_id": asset, "groups": ["luxforge.basic/tone"]}),
    );
    assert_eq!(by_groups["settings"]["set-basic"]["exposure"], json!(1.0));
    // Tone and Auto tone cannot be carried together, whichever way they are named.
    let error = client.error(
        "preset.capture",
        json!({"asset_id": asset, "groups": ["luxforge.basic/tone", "auto-tone"]}),
    );
    assert_eq!(error["code"], json!("validation"), "{error}");
    let error = client.error(
        "preset.capture",
        json!({"asset_id": asset, "groups": ["luxforge.basic/nowhere"]}),
    );
    assert_eq!(error["code"], json!("validation"), "{error}");
    let error = client.error(
        "preset.capture",
        json!({"asset_id": asset, "groups": ["luxforge.basic/tone"], "fields": {"set-basic": true}}),
    );
    assert_eq!(error["code"], json!("validation"), "{error}");
    client.finish();
    std::fs::remove_file(catalog).unwrap();
}

#[test]
fn copy_settings_capture_single_paste_and_batch_share_the_json_contract() {
    let catalog = paths::temp_catalog("copy-settings-cli");
    let mut client = JsonProcess::start(
        env!("CARGO_BIN_EXE_luxforge-json"),
        &["--catalog", catalog.to_str().unwrap()],
        "copy-settings",
    );
    let asset = client.open(&paths::jpeg().canonicalize().unwrap(), ACTOR)["asset"]["id"].clone();
    let settings = client.call(
        "preset.capture",
        json!({"asset_id": asset, "fields": {"set-basic": ["exposure"]}}),
    )["settings"]
        .clone();
    client.call("edit.set-basic", json!({"asset_id": asset, "exposure": 1, "mutation": {"expected_revision": 0, "request_id": request_id("edit"), "actor": ACTOR}}));
    let origin = json!({"kind": "paste", "source": "Original.jpg", "source_asset": asset});
    let pasted = client.call("edit.apply-settings", json!({"asset_id": asset, "settings": settings, "origin": origin, "mutation": {"expected_revision": 1, "request_id": request_id("paste"), "actor": ACTOR}}));
    assert_eq!(pasted["outcome"], "applied");
    assert_eq!(
        client.call("history.list", json!({"asset_id": asset, "limit": 10}))["entries"][0]["label"],
        "Paste settings from Original.jpg"
    );
    let started = client.call("batch.apply-settings", json!({"targets": {"kind": "assets", "asset_ids": [asset]}, "settings": settings, "origin": origin, "mutation": request()}));
    let result = client.settle("job.read", &started["job_id"]);
    assert_eq!(result["status"], "ready", "{result}");
    assert_eq!(
        result["result"]["skipped"].as_array().unwrap().len(),
        1,
        "already has the settings: {result}"
    );
    let refused = client.error("edit.apply-settings", json!({"asset_id": asset, "settings": {"apply-settings": {}}, "origin": {"kind": "paste", "source": "bad"}, "mutation": {"expected_revision": 2, "request_id": request_id("bad"), "actor": ACTOR}}));
    assert_eq!(refused["code"], "validation");
    client.finish();
    std::fs::remove_file(catalog).unwrap();
}
