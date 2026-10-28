//! The installed modules' lifecycle through the catalog owner, across relaunches that read the
//! store and assemble the registry as the desktop and `luxforge-json` do: inspect, install,
//! uninstall, the load states of `module.status` and `module.package.list`, the `module.installed`
//! event, a module that no longer loads keeping its photos' layers while export refuses, a
//! reinstall restoring the same export, and the folder methods existing only in developer mode.
//! The store on its own is `crate::module_store`'s tests; the WebAssembly engine is
//! `luxforge-wasm`'s, and this file's units stand in for it.
use super::*;
use crate::api::ApiFailure;
use crate::export::settings::baseline_json;
use crate::module_package::{
    AdjustmentUnits, ExternalModule, FieldEntry, ModuleEngine, ModulePackage, PackageRefusal,
};
use crate::module_store::{ModuleHost, ModuleStore};
use crate::modules::ColorOperation;
use crate::{CompileStage, RegistryOptions};
use luxforge_test_macros::slow;
use luxforge_testbase::paths::{jpeg as fixture, temp_dir};
use std::fs;

const GLOW: &str = "org.example.glow";
const GLOW_EFFECT: &str = "org.example.glow.adjust";
/// An identity the stand-in engine refuses, as the real one refuses a component that imports.
const IMPORTS: &str = "org.example.imports";

/// The stand-in for the engine's units: every layer compiles to no colour unit.
struct Units;

impl AdjustmentUnits for Units {
    fn compile(&self, _: &[FieldEntry], _: CompileStage) -> Result<ColorOperation, Error> {
        Ok(ColorOperation::new(Vec::new()))
    }
}

/// The stand-in for the engine's check: it refuses [`IMPORTS`] as a component that imports.
struct Engine;

impl ModuleEngine for Engine {
    fn check(&self, package: &ModulePackage) -> Result<(), Error> {
        if package.manifest.id == IMPORTS {
            return Err(PackageRefusal::Component.error(format!(
                "{IMPORTS}: its component imports log, and an adjustment module imports nothing"
            )));
        }
        Ok(())
    }
}

/// One launch over a directory that keeps the catalog, the data root's module store and the
/// package files a client names.
struct Launch {
    owner: OwnerHandle,
    join: Option<JoinHandle<()>>,
    client: ClientId,
    dir: PathBuf,
    developer: bool,
    keep: bool,
}

impl Launch {
    fn first(name: &str, developer: bool) -> Self {
        let dir = temp_dir(&format!("module-install-{name}"))
            .canonicalize()
            .unwrap();
        Self::start(dir, developer)
    }

    /// Read the store and assemble the registry as a launch does, then start the owner.
    fn start(dir: PathBuf, developer: bool) -> Self {
        let store = ModuleStore::new(dir.join("data").join("modules"));
        let read = store.launch(developer);
        let external: Vec<ExternalModule> = read
            .modules
            .iter()
            .filter_map(|module| {
                let external = match &module.read {
                    Ok(package) => ExternalModule::loaded(&package.manifest, Arc::new(Units)),
                    Err(unread) => {
                        ExternalModule::unavailable(unread.manifest.as_ref()?, &unread.reason)
                    }
                };
                Some(external.ok()?.with_origin(module.origin))
            })
            .collect();
        let registry = ModuleRegistry::assemble(&RegistryOptions {
            developer,
            external: &external,
            ..RegistryOptions::default()
        })
        .expect("a launch never fails because of a package");
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(registry),
            HostConfig {
                modules: ModuleHost {
                    store: Some(store),
                    engine: Some(Arc::new(Engine)),
                    launch: Arc::new(read.record()),
                },
                ..HostConfig::unconfigured()
            },
        )
        .unwrap();
        let client = owner.register();
        Self {
            owner,
            join: Some(join),
            client,
            dir,
            developer,
            keep: false,
        }
    }

    /// Quit and launch again over the same directory, in developer mode or not.
    fn relaunch(mut self, developer: bool) -> Self {
        self.keep = true;
        self.owner.stop();
        if let Some(join) = self.join.take() {
            join.join().unwrap();
        }
        Self::start(self.dir.clone(), developer)
    }

    fn send(&self, id: &str, method: &str, params: Value) -> ApiResponse {
        self.owner
            .call(
                self.client,
                ApiRequest {
                    id: id.into(),
                    method: method.into(),
                    params,
                    token: None,
                },
            )
            .expect("the owner answered")
    }

    fn ok(&self, id: &str, method: &str, params: Value) -> Value {
        let response = self.send(id, method, params);
        assert!(response.error.is_none(), "{method}: {:?}", response.error);
        response.result.expect("a result")
    }

    fn refused(&self, id: &str, method: &str, params: Value) -> ApiFailure {
        self.send(id, method, params)
            .error
            .unwrap_or_else(|| panic!("{method} was expected to be refused"))
    }

    /// The request ids announced as `module.installed`, in order.
    fn announced(&self) -> Vec<String> {
        let events = self.ok("events", "events.since", json!({"after": 0}));
        events["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|event| event["method"] == "module.installed")
            .map(|event| event["request_id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Write a package of `id` at `version` beside the catalog and answer its path.
    fn package(&self, file: &str, id: &str, name: &str, version: &str, format: u32) -> PathBuf {
        let manifest =
            crate::module_store::module_install_tests::manifest(id, name, version, format);
        let path = self.dir.join(file);
        fs::write(
            &path,
            crate::module_store::module_install_tests::package_bytes(&manifest, true),
        )
        .unwrap();
        path
    }

    /// The external module's load state in `module.status`.
    fn external(&self, id: &str) -> Value {
        self.ok("status", "module.status", json!({"module_id": id}))["external"].clone()
    }

    /// The origin `module.list` lists `id` with, or `None` when it is not registered.
    fn listed(&self, id: &str) -> Option<String> {
        self.ok("list", "module.list", json!({}))["modules"]
            .as_array()
            .unwrap()
            .iter()
            .find(|module| module["id"] == id)
            .map(|module| module["origin"].as_str().unwrap().to_owned())
    }

    fn methods(&self) -> Value {
        self.ok("schema", "schema.list", json!({}))["methods"].clone()
    }

    /// Copy the fixture beside the catalog, develop and prepare it: the photograph's id.
    fn import(&self) -> Value {
        let path = self.dir.join("photo.jpg");
        fs::copy(fixture(), &path).unwrap();
        super::library::opening::import(&self.owner, self.client, &path)
    }

    fn revision(&self, asset: &Value) -> u64 {
        self.ok("state", "asset.state", json!({"asset_id": asset}))["revision"]
            .as_u64()
            .unwrap()
    }

    /// The effect of each layer of the photograph's current entry, and whether it is available.
    fn layers(&self, asset: &Value) -> Vec<(String, bool)> {
        self.ok("describe", "recipe.describe", json!({"asset_id": asset}))["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|layer| {
                (
                    layer["effect"].as_str().unwrap().to_owned(),
                    layer["available"].as_bool().unwrap(),
                )
            })
            .collect()
    }

    /// Prepare the photograph's source, as a launch's first render of it needs.
    fn prepare(&self, asset: &Value) {
        let started = self.ok("prepare", "source.prepare", json!({"asset_id": asset}));
        let job = started["job_id"].clone();
        let done = self.settle(&job, "the source to be prepared");
        assert_eq!(done["status"], "ready", "{done}");
    }

    fn settle(&self, job: &Value, what: &str) -> Value {
        luxforge_testbase::wait_for(what, || {
            let read = self.ok("job", "job.read", json!({"job_id": job}));
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        })
    }

    /// Export the photograph and answer the file's bytes.
    fn export(&self, asset: &Value, name: &str) -> Vec<u8> {
        self.prepare(asset);
        let destination = self.dir.join(name);
        let started = self.ok(
            name,
            "export.write",
            json!({
                "asset_id": asset,
                "destination": destination,
                "settings": baseline_json(),
                "mutation": {"request_id": name, "actor": "test"},
            }),
        );
        let done = self.settle(&started["job_id"], "the export to settle");
        assert_eq!(done["status"], "ready", "{done}");
        fs::read(destination).unwrap()
    }

    /// What `export.plan` refuses the photograph with.
    fn export_refused(&self, asset: &Value) -> ApiFailure {
        self.refused(
            "plan",
            "export.plan",
            json!({"asset_id": asset, "settings": baseline_json()}),
        )
    }
}

impl Drop for Launch {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        if !self.keep {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }
}

/// Inspect, install and relaunch: the module registers as `community`, edits and exports; an
/// uninstall leaves the photo's layer as it is and, after a relaunch, export refuses it naming the
/// effect; reinstalling restores the same export with no data change; a stored package broken on
/// disk registers unavailable with its reason, keeping the layer. Each write answers
/// `pending-relaunch` and announces `module.installed`; a write that changes nothing announces
/// nothing.
#[test]
#[slow]
fn module_install_lifecycle_across_relaunches_keeps_every_layer() {
    let launch = Launch::first("lifecycle", false);
    let path = launch.package("glow.lfmodule", GLOW, "glow", "1.0.0", 1);

    let inspected = launch.ok("inspect", "module.package.inspect", json!({"path": path}));
    assert_eq!(inspected["manifest"]["id"], GLOW);
    assert_eq!(
        inspected["methods"],
        json!(["edit.set-glow", "edit.reset-glow"])
    );
    assert_eq!(inspected["placements"], json!([{"toolset": "adjustments"}]));
    assert_eq!(
        inspected["access"],
        json!({"network": "none", "files": "none"})
    );
    assert_eq!(
        (
            &inspected["program"],
            &inspected["reviewed"],
            &inspected["installed"]
        ),
        (&json!(true), &json!(false), &Value::Null)
    );
    assert_eq!(inspected["status"], Value::Null);
    assert!(launch.announced().is_empty(), "inspecting writes nothing");
    assert!(
        !launch.dir.join("data").exists(),
        "inspecting creates nothing"
    );

    let installed = launch.ok("install", "module.install", json!({"path": path}));
    assert_eq!(installed["outcome"], "installed");
    assert_eq!(installed["origin"], "community");
    assert_eq!(installed["status"]["state"], "pending-relaunch");
    assert_eq!(installed["status"]["running"], Value::Null);
    assert_eq!(installed["format_change"], Value::Null);
    let again = launch.ok("install-again", "module.install", json!({"path": path}));
    assert_eq!(again["outcome"], "unchanged");
    assert_eq!(
        launch.announced(),
        ["install"],
        "an unchanged install announces nothing"
    );
    let listing = launch.ok("packages", "module.package.list", json!({}));
    assert_eq!(listing["pending_relaunch"], true);
    assert_eq!(listing["store"], json!({"state": "ready"}));
    assert_eq!(
        launch.ok("status", "module.status", json!({"module_id": GLOW})),
        json!({"module_id": GLOW, "external": installed["status"]}),
        "a module stored since launch answers its load state alone"
    );
    assert_eq!(
        launch.listed(GLOW),
        None,
        "nothing is registered before the relaunch"
    );
    assert!(launch.methods().get("edit.set-glow").is_none());
    let asset = launch.import();

    // The next launch loads it.
    let launch = launch.relaunch(false);
    assert_eq!(launch.listed(GLOW).as_deref(), Some("community"));
    let loaded = launch.external(GLOW);
    assert_eq!(loaded["state"], "loaded", "{loaded}");
    assert_eq!(loaded["program"], json!({"state": "present"}));
    assert_eq!(loaded["installed"], loaded["running"]);
    assert_eq!(
        launch.ok("packages", "module.package.list", json!({}))["pending_relaunch"],
        false
    );
    launch.ok(
        "glow",
        "edit.set-glow",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": launch.revision(&asset), "request_id": "glow", "actor": "test"},
            "amount": 40,
        }),
    );
    let revision = launch.revision(&asset);
    let layers = launch.layers(&asset);
    assert!(
        layers.contains(&(GLOW_EFFECT.to_owned(), true)),
        "{layers:?}"
    );
    let first = launch.export(&asset, "first.jpg");

    // Uninstalled: pending until the relaunch, after which the layer is kept and export refuses.
    let removed = launch.ok("uninstall", "module.uninstall", json!({"module": GLOW}));
    assert_eq!(removed["status"]["state"], "pending-relaunch");
    assert_eq!(removed["status"]["installed"], Value::Null);
    assert_eq!(launch.announced(), ["uninstall"]);
    let launch = launch.relaunch(false);
    assert_eq!(launch.listed(GLOW), None);
    assert!(
        launch.ok("packages", "module.package.list", json!({}))["modules"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(launch.revision(&asset), revision);
    assert!(
        launch
            .layers(&asset)
            .contains(&(GLOW_EFFECT.to_owned(), false))
    );
    let refused = launch.export_refused(&asset);
    assert_eq!(refused.code, "incompatible", "{refused:?}");
    assert_eq!(refused.data.as_ref().unwrap()["effect_id"], GLOW_EFFECT);
    assert!(
        refused.message.contains("unavailable effect"),
        "{}",
        refused.message
    );

    // Reinstalled: the same export, with no data change.
    let reinstalled = launch.ok("reinstall", "module.install", json!({"path": path}));
    assert_eq!(reinstalled["outcome"], "installed");
    let launch = launch.relaunch(false);
    assert_eq!(launch.revision(&asset), revision);
    assert_eq!(launch.export(&asset, "again.jpg"), first);

    // The stored package broken on disk: unavailable with the reason, the layer kept.
    let store = ModuleStore::new(launch.dir.join("data").join("modules"));
    let stored = store.read().unwrap().packages[0].clone();
    fs::write(
        store.package_dir(&stored).join("package.lfmodule"),
        b"not a zip",
    )
    .unwrap();
    let launch = launch.relaunch(false);
    assert_eq!(launch.listed(GLOW).as_deref(), Some("community"));
    let failed = launch.external(GLOW);
    assert_eq!(failed["state"], "failed", "{failed}");
    assert!(!failed["reason"].as_str().unwrap().is_empty());
    assert_eq!(launch.revision(&asset), revision);
    let refused = launch.export_refused(&asset);
    assert_eq!(refused.data.as_ref().unwrap()["effect_id"], GLOW_EFFECT);
    let edit = launch.refused(
        "glow-again",
        "edit.set-glow",
        json!({
            "asset_id": asset,
            "mutation": {"expected_revision": revision, "request_id": "glow-again", "actor": "test"},
            "amount": 10,
        }),
    );
    assert_ne!(
        edit.code, "protocol",
        "the action is still registered: {edit:?}"
    );
    assert_eq!(launch.revision(&asset), revision);
}

/// A new version whose payload format differs says so; it replaces the installed one at the next
/// launch, where the old layer's format is refused by its marker and kept.
#[test]
fn module_install_of_another_payload_format_warns_and_replaces_at_relaunch() {
    let launch = Launch::first("format", false);
    let first = launch.package("glow-1.lfmodule", GLOW, "glow", "1.0.0", 1);
    launch.ok("install", "module.install", json!({"path": first}));
    let second = launch.package("glow-2.lfmodule", GLOW, "glow", "2.0.0", 2);
    let inspected = launch.ok("inspect", "module.package.inspect", json!({"path": second}));
    assert_eq!(inspected["installed"]["version"], "1.0.0");
    assert_eq!(inspected["installed"]["origin"], "community");
    assert_eq!(
        (
            &inspected["format_change"]["installed"],
            &inspected["format_change"]["package"]
        ),
        (&json!(1), &json!(2))
    );
    let replaced = launch.ok("replace", "module.install", json!({"path": second}));
    assert_eq!(replaced["outcome"], "replaced");
    assert_eq!(replaced["previous"]["version"], "1.0.0");
    assert!(
        replaced["format_change"]["notice"]
            .as_str()
            .unwrap()
            .contains("unavailable until a version that reads them is installed")
    );
    let launch = launch.relaunch(false);
    let status = launch.external(GLOW);
    assert_eq!(
        (&status["state"], &status["running"]["version"]),
        (&json!("loaded"), &json!("2.0.0"))
    );
}

/// Bad input is refused by name and announces nothing: a relative path, a file that is not a
/// package, a component the engine would not load, an action a built-in module provides, an id
/// the store does not hold; and a host without a store refuses every write `not-ready`.
#[test]
fn module_install_refuses_bad_input_by_name() {
    let launch = Launch::first("refusals", false);
    let relative = launch.refused(
        "relative",
        "module.install",
        json!({"path": "glow.lfmodule"}),
    );
    assert_eq!(relative.code, "validation");
    let text = launch.dir.join("notes.lfmodule");
    fs::write(&text, "not a package").unwrap();
    for method in ["module.package.inspect", "module.install"] {
        let refused = launch.refused("text", method, json!({"path": text}));
        assert_eq!(refused.code, "validation", "{refused:?}");
        assert_eq!(refused.data.as_ref().unwrap()["refusal"], "archive");
    }
    let imports = launch.package("imports.lfmodule", IMPORTS, "imports", "1.0.0", 1);
    let refused = launch.refused("imports", "module.install", json!({"path": imports}));
    assert_eq!(refused.data.as_ref().unwrap()["refusal"], "component");
    assert!(
        refused.message.contains("imports log"),
        "{}",
        refused.message
    );
    let collides = launch.package(
        "shade.lfmodule",
        "org.example.shade",
        "vignette",
        "1.0.0",
        1,
    );
    let refused = launch.refused(
        "collides",
        "module.package.inspect",
        json!({"path": collides}),
    );
    assert_eq!(refused.data.as_ref().unwrap()["refusal"], "collision");
    assert!(
        refused.message.contains("luxforge.vignette"),
        "{}",
        refused.message
    );
    let unknown = launch.refused("unknown", "module.uninstall", json!({"module": GLOW}));
    assert_eq!(unknown.code, "validation");
    assert!(unknown.message.contains(GLOW), "{}", unknown.message);
    assert!(launch.announced().is_empty());
    assert!(
        !launch
            .dir
            .join("data")
            .join("modules")
            .join("packages")
            .exists()
    );

    let (owner, join) = OwnerHandle::start_with_host(
        &launch.dir.join("bare.sqlite"),
        Arc::new(ModuleRegistry::builtin()),
        HostConfig::unconfigured(),
    )
    .unwrap();
    let client = owner.register();
    let response = owner
        .call(
            client,
            ApiRequest {
                id: "bare".into(),
                method: "module.install".into(),
                params: json!({"path": imports}),
                token: None,
            },
        )
        .unwrap();
    assert_eq!(response.error.unwrap().code, "not-ready");
    owner.stop();
    join.join().unwrap();
}

/// The folder methods exist only in developer mode: absent from `schema.list` and unknown to
/// dispatch otherwise. A developer launch loads the folder as `folder`; an ordinary launch over the
/// same store neither loads nor lists it, and keeps it stored.
#[test]
fn module_install_folder_methods_exist_only_in_developer_mode() {
    let launch = Launch::first("folders", false);
    let methods = launch.methods();
    for method in ["module.folder.load", "module.folder.remove"] {
        assert!(
            methods.get(method).is_none(),
            "{method} is listed outside developer mode"
        );
        let refused = launch.refused(method, method, json!({"path": "/x", "module": GLOW}));
        assert!(refused.message.starts_with("unknown method"), "{refused:?}");
    }
    for method in [
        "module.package.inspect",
        "module.install",
        "module.uninstall",
        "module.package.list",
    ] {
        assert!(methods.get(method).is_some(), "{method}");
    }

    let launch = launch.relaunch(true);
    assert!(launch.methods().get("module.folder.load").is_some());
    let folder = launch.dir.join("glow-module");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join(crate::module_package::MANIFEST_FILE),
        crate::module_store::module_install_tests::manifest(GLOW, "glow", "0.1.0", 1),
    )
    .unwrap();
    fs::write(
        folder.join(crate::module_package::COMPONENT_FILE),
        crate::module_store::module_install_tests::COMPONENT,
    )
    .unwrap();
    let loaded = launch.ok("load", "module.folder.load", json!({"path": folder}));
    assert_eq!(
        (&loaded["outcome"], &loaded["origin"]),
        (&json!("loaded"), &json!("folder"))
    );
    assert_eq!(loaded["status"]["state"], "pending-relaunch");
    assert_eq!(launch.announced(), ["load"]);

    let launch = launch.relaunch(true);
    assert_eq!(launch.listed(GLOW).as_deref(), Some("folder"));
    assert_eq!(launch.external(GLOW)["state"], "loaded");
    assert!(launch.developer);

    let launch = launch.relaunch(false);
    assert_eq!(launch.listed(GLOW), None);
    assert!(
        launch.ok("packages", "module.package.list", json!({}))["modules"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let launch = launch.relaunch(true);
    let removed = launch.ok("remove", "module.folder.remove", json!({"module": GLOW}));
    assert_eq!(removed["status"]["state"], "pending-relaunch");
    assert!(folder.join(crate::module_package::COMPONENT_FILE).is_file());
    let launch = launch.relaunch(true);
    assert_eq!(launch.listed(GLOW), None);
}
