//! Installed external modules over `luxforge-json`'s pipe, across relaunches of the binary over one
//! data root, with a real component run by the WebAssembly engine: `luxforge-wasm`'s test
//! component, a gain-and-lift adjustment written in WAT. An install is checked by the engine and
//! takes effect at the next launch; the module edits and exports; uninstalled, the photo keeps its
//! layer and export refuses naming the effect; reinstalled, the export is the same file; a stored
//! package broken on disk registers unavailable with its reason; and the folder methods exist only
//! with `--developer`.
use luxforge_test_macros::slow;
use luxforge_testbase::paths;
use luxforge_testkit::JsonProcess;
use serde_json::{Value, json};
use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
};

#[allow(dead_code)]
#[path = "../../../luxforge-wasm/tests/host/components.rs"]
mod components;

const BINARY: &str = env!("CARGO_BIN_EXE_luxforge-json");
const MODULE: &str = "org.example.gainlift";

/// A `.lfmodule` of the test manifest and `variant`'s component.
fn package(path: &Path, variant: components::Variant) {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [
        (
            "luxforge-module.toml",
            components::MANIFEST.as_bytes().to_vec(),
        ),
        ("module.wasm", components::component(variant)),
    ] {
        zip.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(&bytes).unwrap();
    }
    std::fs::write(path, zip.finish().unwrap().into_inner()).unwrap();
}

struct Root {
    dir: PathBuf,
}

impl Root {
    fn new(name: &str) -> Self {
        let dir = paths::temp_path(name);
        std::fs::create_dir_all(&dir).unwrap();
        Self {
            dir: dir.canonicalize().unwrap(),
        }
    }

    /// One `luxforge-json` launch over the data root's catalog and module store.
    fn launch(&self, developer: bool) -> JsonProcess {
        let catalog = self.dir.join("catalog.sqlite");
        let mut args = vec![
            "--catalog",
            catalog.to_str().unwrap(),
            "--data-root",
            self.dir.to_str().unwrap(),
            "--secret-store",
            "memory",
        ];
        if developer {
            args.push("--developer");
        }
        let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        JsonProcess::start(BINARY, &args, "modules")
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn origin(client: &mut JsonProcess) -> Option<String> {
    client.call("module.list", json!({}))["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|module| module["id"] == MODULE)
        .map(|module| module["origin"].as_str().unwrap().to_owned())
}

fn external(client: &mut JsonProcess) -> Value {
    client.call("module.status", json!({"module_id": MODULE}))["external"].clone()
}

fn prepare(client: &mut JsonProcess, asset: &Value) {
    let started = client.call("source.prepare", json!({"asset_id": asset}));
    if !started["job_id"].is_null() {
        let prepared = client.settle("job.read", &started["job_id"]);
        assert_eq!(prepared["status"], "ready", "{prepared}");
    }
}

/// Today's export settings, without metadata.
fn settings() -> Value {
    json!(luxforge_core::ExportSettings::baseline(
        luxforge_core::MetadataLevel::None
    ))
}

fn export(client: &mut JsonProcess, asset: &Value, destination: &Path) -> Vec<u8> {
    prepare(client, asset);
    let started = client.call(
        "export.write",
        json!({
            "asset_id": asset,
            "destination": destination,
            "settings": settings(),
            "mutation": {"request_id": destination.file_name().unwrap().to_str().unwrap(), "actor": "cli-test"},
        }),
    );
    let done = client.settle("job.read", &started["job_id"]);
    assert_eq!(done["status"], "ready", "{done}");
    std::fs::read(destination).unwrap()
}

fn revision(client: &mut JsonProcess, asset: &Value) -> u64 {
    client.call("asset.state", json!({"asset_id": asset}))["revision"]
        .as_u64()
        .unwrap()
}

#[test]
#[slow]
fn json_installs_a_module_for_the_next_launch_and_keeps_its_layers_when_it_is_removed() {
    let root = Root::new("json-cli-modules");
    let valid = root.dir.join("gainlift.lfmodule");
    package(&valid, components::Variant::Valid);
    let imports = root.dir.join("imports.lfmodule");
    package(&imports, components::Variant::Imports);
    let photo = root.dir.join("photo.jpg");
    std::fs::copy(paths::jpeg(), &photo).unwrap();

    let mut client = root.launch(false);
    let inspected = client.call("module.package.inspect", json!({"path": valid}));
    assert_eq!(inspected["manifest"]["id"], MODULE);
    assert_eq!(inspected["program"], false);
    let refused = client.error("module.install", json!({"path": imports}));
    assert_eq!(refused["data"]["refusal"], "component", "{refused}");
    let installed = client.call("module.install", json!({"path": valid}));
    assert_eq!(installed["status"]["state"], "pending-relaunch");
    assert_eq!(external(&mut client)["state"], "pending-relaunch");
    assert_eq!(origin(&mut client), None);
    let schema = client.call("schema.list", json!({}));
    assert!(schema["methods"].get("edit.set-gainlift").is_none());
    assert!(schema["methods"].get("module.folder.load").is_none());
    let events = client.call("events.since", json!({"after": 0}));
    assert!(
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["method"] == "module.installed"),
        "{events}"
    );
    let asset = client.open(&photo, "cli-test")["asset"]["id"].clone();
    client.finish();

    // The next launch loads it from the store.
    let mut client = root.launch(false);
    assert_eq!(origin(&mut client).as_deref(), Some("community"));
    let loaded = external(&mut client);
    assert_eq!(loaded["state"], "loaded", "{loaded}");
    assert_eq!(loaded["program"], json!({"state": "none"}));
    let schema = client.call("schema.list", json!({}));
    assert!(schema["methods"]["edit.set-gainlift"].is_object());
    let current = revision(&mut client, &asset);
    client.call(
        "edit.set-gainlift",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": current, "request_id": "gain", "actor": "cli-test"},
            "gain": 20,
            "lift": 5,
        }),
    );
    let edited = revision(&mut client, &asset);
    let first = export(&mut client, &asset, &root.dir.join("first.jpg"));
    let removed = client.call("module.uninstall", json!({"module": MODULE}));
    assert_eq!(removed["status"]["state"], "pending-relaunch");
    client.finish();

    // Uninstalled: the layer is kept and export refuses, naming the effect.
    let mut client = root.launch(false);
    assert_eq!(origin(&mut client), None);
    assert_eq!(revision(&mut client, &asset), edited);
    prepare(&mut client, &asset);
    let refused = client.error(
        "export.plan",
        json!({"asset_id": asset, "settings": settings()}),
    );
    assert_eq!(refused["code"], "incompatible", "{refused}");
    assert_eq!(refused["data"]["effect_id"], format!("{MODULE}.adjust"));
    client.call("module.install", json!({"path": valid}));
    client.finish();

    // Reinstalled: the same export, with no data change.
    let mut client = root.launch(false);
    assert_eq!(revision(&mut client, &asset), edited);
    assert_eq!(
        export(&mut client, &asset, &root.dir.join("again.jpg")),
        first
    );
    client.finish();

    // Broken on disk: listed, unavailable with the reason, the layer kept.
    let packages = root
        .dir
        .join("data")
        .join("modules")
        .join("packages")
        .join(MODULE);
    let version = std::fs::read_dir(&packages)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(version.join("package.lfmodule"), b"truncated").unwrap();
    let mut client = root.launch(false);
    assert_eq!(origin(&mut client).as_deref(), Some("community"));
    let failed = external(&mut client);
    assert_eq!(failed["state"], "failed", "{failed}");
    assert_eq!(revision(&mut client, &asset), edited);
    prepare(&mut client, &asset);
    let refused = client.error(
        "export.plan",
        json!({"asset_id": asset, "settings": settings()}),
    );
    assert_eq!(refused["data"]["effect_id"], format!("{MODULE}.adjust"));
    client.finish();
}

#[test]
fn json_folder_methods_exist_only_with_developer() {
    let root = Root::new("json-cli-module-folders");
    let folder = root.dir.join("gainlift");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("luxforge-module.toml"), components::MANIFEST).unwrap();
    std::fs::write(
        folder.join("module.wasm"),
        components::component(components::Variant::Valid),
    )
    .unwrap();

    let mut client = root.launch(false);
    let refused = client.error("module.folder.load", json!({"path": folder}));
    assert!(
        refused["message"]
            .as_str()
            .unwrap()
            .starts_with("unknown method"),
        "{refused}"
    );
    client.finish();

    let mut client = root.launch(true);
    let loaded = client.call("module.folder.load", json!({"path": folder}));
    assert_eq!(loaded["origin"], "folder");
    client.finish();
    let mut client = root.launch(true);
    assert_eq!(origin(&mut client).as_deref(), Some("folder"));
    assert_eq!(external(&mut client)["state"], "loaded");
    client.finish();
    let mut client = root.launch(false);
    assert_eq!(origin(&mut client), None);
    client.finish();
}
