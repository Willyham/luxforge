//! The packages the steps install and the data roots they install them into, through the API's
//! `module.install` on a catalog owner whose host holds the store, exactly as `luxforge-json` and
//! the desktop assemble it (`luxforge_cli::installed_modules`): the store under the data root, each
//! stored package loaded by the module engine with its compiled code cached there, and the engine
//! as the store's install check. A data root here has the layout `luxforge_cli::Paths::resolve`
//! gives `--data-root`: `config/` (the catalog), `data/modules/` (the store, the compiled code and
//! the probes) and `logs/`.
use super::{ACTOR, FIVE};
use crate::*;
use luxforge_core::{
    HostConfig, ModuleRegistry, RegistryOptions,
    module_package::{ExternalModule, ModuleEngine, ModulePackage, PackageRefusal},
    module_store::{ModuleHost, ModuleStore},
    preferences::CATALOG_FILE,
};
use luxforge_testkit::client::Owner;
use luxforge_wasm::{Programs, WasmHost};
use std::sync::Arc;

/// The example's directory, which is only read.
const EXAMPLE: &str = "examples/modules/split-toning";
/// The example's identity, name and title, which the repackagings replace.
const ID: &str = "org.example.splittoning";
const NAME: &str = "split-toning";
const TITLE: &str = "Split toning";

/// One package file and what it holds.
pub(crate) struct Built {
    pub path: PathBuf,
    pub package: ModulePackage,
}

/// The example built, and its repackagings under other identities.
pub(crate) struct Packages {
    pub all: Vec<Built>,
}

/// The example's manifest under the `index`th other identity: its id, name and title each with
/// the index appended, so its actions, effect and title are its own.
pub(crate) fn renamed(manifest: &str, index: usize) -> Result<String> {
    let mut text = String::with_capacity(manifest.len() + 16);
    let mut replaced = [false; 3];
    for line in manifest.lines() {
        let line = match line {
            line if line == format!("id = \"{ID}\"") => {
                replaced[0] = true;
                format!("id = \"{ID}{index}\"")
            }
            line if line == format!("name = \"{NAME}\"") => {
                replaced[1] = true;
                format!("name = \"{NAME}-{index}\"")
            }
            line if line == format!("title = \"{TITLE}\"") => {
                replaced[2] = true;
                format!("title = \"{TITLE} {index}\"")
            }
            line => line.to_owned(),
        };
        text.push_str(&line);
        text.push('\n');
    }
    ensure(
        replaced.iter().all(|done| *done),
        "the example's manifest no longer declares its id, name and title as module-measure \
         expects",
    )?;
    Ok(text)
}

impl Packages {
    /// Build the example with `module-package` into `dir`, then package its component and program
    /// under [`FIVE`] − 1 other identities beside it.
    pub fn build(root: &Path, dir: &Path) -> Result<Self> {
        let first = dir.join("split-toning.lfmodule");
        let package = crate::module_package::run(root, &root.join(EXAMPLE), &first)?;
        let manifest = fs::read_to_string(root.join(EXAMPLE).join("luxforge-module.toml"))?;
        let mut all = vec![Built {
            path: first,
            package: package.clone(),
        }];
        for index in 2..=FIVE {
            let path = dir.join(format!("split-toning-{index}.lfmodule"));
            let bytes = crate::module_package::archive(
                renamed(&manifest, index)?.as_bytes(),
                &package.component,
                package.program.as_deref().map(str::as_bytes),
            )?;
            fs::write(&path, bytes)?;
            let package = ModulePackage::read_file(&path).map_err(|error| error.detail)?;
            all.push(Built { path, package });
        }
        Ok(Self { all })
    }

    /// The example itself.
    pub fn first(&self) -> &Built {
        &self.all[0]
    }

    pub fn describe(&self) -> Value {
        json!(
            self.all
                .iter()
                .map(|built| json!({
                    "path": built.path,
                    "id": built.package.manifest.id,
                    "hash": built.package.hash,
                    "component_bytes": built.package.component.len(),
                    "program": built.package.program.is_some(),
                    "package_bytes": fs::metadata(&built.path).map(|m| m.len()).ok(),
                }))
                .collect::<Vec<_>>()
        )
    }
}

/// One data root, laid out as `--data-root` lays it out.
#[derive(Clone, Debug)]
pub(crate) struct DataRoot {
    pub dir: PathBuf,
}

impl DataRoot {
    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.into() }
    }
    fn config(&self) -> PathBuf {
        self.dir.join("config")
    }
    fn data(&self) -> PathBuf {
        self.dir.join("data")
    }
    pub fn store(&self) -> PathBuf {
        self.data().join("modules")
    }
    pub fn code_cache(&self) -> PathBuf {
        self.store().join("compiled")
    }
    pub fn probe_cache(&self) -> PathBuf {
        self.store().join("probes")
    }
    pub fn catalog(&self) -> PathBuf {
        self.config().join(CATALOG_FILE)
    }
    pub fn events(&self) -> PathBuf {
        self.dir.join("logs").join("events.jsonl")
    }
}

/// The engine as the store's install check, as `luxforge_cli::installed_modules` hands it over: a
/// component loads as a launch would load it, or the package is refused with the engine's reason.
struct Engine(WasmHost);

impl ModuleEngine for Engine {
    fn check(&self, package: &ModulePackage) -> std::result::Result<(), luxforge_core::Error> {
        self.0
            .load(package)
            .map(drop)
            .map_err(|error| PackageRefusal::Component.error(error.detail))
    }
}

/// What one launch reads from `root`'s store: the external modules, loaded by an engine caching
/// under the root (registering their programs with `programs` when given), and the module host.
pub(crate) fn launch_modules(
    root: &DataRoot,
    programs: Option<Arc<Programs>>,
) -> (Vec<ExternalModule>, ModuleHost) {
    let engine = WasmHost::new(Some(root.code_cache()));
    let loader = match programs {
        Some(programs) => engine.clone().with_programs(programs),
        None => engine.clone(),
    };
    let store = ModuleStore::new(root.store());
    let launch = store.launch(false);
    let external = launch
        .modules
        .iter()
        .filter_map(|module| {
            let external = match &module.read {
                Ok(package) => loader.external(package).ok()?,
                Err(unread) => {
                    ExternalModule::unavailable(unread.manifest.as_ref()?, &unread.reason).ok()?
                }
            };
            Some(external.with_origin(module.origin))
        })
        .collect();
    let host = ModuleHost {
        store: Some(store),
        engine: Some(Arc::new(Engine(engine))),
        launch: Arc::new(launch.record()),
    };
    (external, host)
}

/// A catalog owner on `root`, as a launch over it starts one: its registry with the store's
/// modules after the linked ones, and its host holding the store.
pub(crate) fn owner(root: &DataRoot) -> Result<Owner> {
    let (external, modules) = launch_modules(root, None);
    let registry = ModuleRegistry::assemble(&RegistryOptions {
        disabled: &[],
        developer: false,
        proof_endpoint: None,
        external: &external,
    })?;
    let host = HostConfig {
        preferences_dir: Some(root.config()),
        config_dir: Some(root.config().join("modules")),
        resource_dir: Some(root.store().join("resources")),
        modules,
        ..HostConfig::unconfigured()
    };
    fs::create_dir_all(root.config())?;
    Ok(Owner::start_with_host(
        &root.catalog(),
        registry,
        host,
        ACTOR,
    )?)
}

/// Install `packages` into `root` through `module.install`, then start the next launch's owner and
/// check its `module.package.list` answers each one loaded, which it returns. With no packages the
/// root holds an empty catalog and no store.
pub(crate) fn install(root: &DataRoot, packages: &[&Built]) -> Result<Value> {
    let owner = owner(root)?;
    let client = owner.client();
    for built in packages {
        let answer = owner.call(
            client,
            "module.install",
            json!({"path": built.path.canonicalize()?}),
        )?;
        ensure(
            answer["outcome"] == "installed",
            format!("module.install answered {answer}"),
        )?;
    }
    owner.close()?;
    let next = self::owner(root)?;
    let client = next.client();
    let listed = next.call(client, "module.package.list", json!({}))?;
    next.close()?;
    let rows = listed["modules"].as_array().map_or(&[][..], Vec::as_slice);
    ensure(
        rows.len() == packages.len(),
        format!(
            "the next launch lists {} external modules, not {}: {listed}",
            rows.len(),
            packages.len()
        ),
    )?;
    for built in packages {
        let id = &built.package.manifest.id;
        ensure(
            rows.iter()
                .any(|row| row["module"] == json!(id) && row["state"] == "loaded"),
            format!("the next launch did not load {id}: {listed}"),
        )?;
    }
    Ok(listed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str =
        include_str!("../../../examples/modules/split-toning/luxforge-module.toml");

    /// A repackaging's manifest is the example's with only its identity, name and title changed,
    /// and the core's manifest reader accepts it.
    #[test]
    fn a_renamed_manifest_is_its_own_module() {
        let text = renamed(MANIFEST, 3).unwrap();
        let manifest = luxforge_core::module_package::ModuleManifest::parse(&text).unwrap();
        assert_eq!(manifest.id, "org.example.splittoning3");
        assert_eq!(manifest.title, "Split toning 3");
        assert_ne!(
            manifest.effect.id,
            luxforge_core::module_package::ModuleManifest::parse(MANIFEST)
                .unwrap()
                .effect
                .id
        );
        let changed = MANIFEST
            .lines()
            .zip(text.lines())
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(changed, 3);
        assert_eq!(MANIFEST.lines().count(), text.lines().count());
        assert!(renamed("id = \"other\"\n", 2).is_err());
    }

    #[test]
    fn a_data_root_is_laid_out_as_the_launch_resolves_it() {
        let root = DataRoot::new(Path::new("/r"));
        assert_eq!(root.store(), Path::new("/r/data/modules"));
        assert_eq!(root.code_cache(), Path::new("/r/data/modules/compiled"));
        assert_eq!(root.probe_cache(), Path::new("/r/data/modules/probes"));
        assert_eq!(root.catalog(), Path::new("/r/config/catalog.sqlite"));
        assert_eq!(root.events(), Path::new("/r/logs/events.jsonl"));
    }
}
