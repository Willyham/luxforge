//! The headless command lines and what they share with the desktop: the `luxforge-json` binary,
//! which serves one JSON-lines client over its standard streams; the `luxforge-ctl` binary, a
//! client of the desktop's live session ([`live`]); the MCP adapter's tool schemas and request
//! check, generated from `schema.list` ([`mcp`]); and [`Paths`], where the application keeps its
//! files, which the desktop and these binaries resolve the same way. It depends on no GUI crate, so
//! building the headless binaries builds no window, renderer or dialog stack.
pub mod live;
pub mod mcp;
mod paths;

pub use paths::Paths;

use luxforge_core::{
    module_package::{ExternalModule, ModuleEngine, ModulePackage, PackageRefusal},
    module_store::{ModuleHost, ModuleStore, StoreLaunch},
};
use std::sync::Arc;

/// The application version every executable's `--version` reports,
/// [`luxforge_core::APPLICATION_VERSION`]: the release tag's normalized version in a tag build,
/// otherwise the workspace version.
pub const VERSION: &str = luxforge_core::APPLICATION_VERSION;

/// What a launch reads from the installed modules' store, for the desktop and `luxforge-json`
/// alike ([`installed_modules`]): the external modules to hand the registry assembly
/// ([`luxforge_core::RegistryOptions::external`]) and the module host to give the catalog owner
/// ([`luxforge_core::HostConfig::modules`]).
pub struct InstalledModules {
    pub external: Vec<ExternalModule>,
    pub host: ModuleHost,
}

/// Read the installed modules' store under `paths`' data root ([`Paths::module_store`]), and in a
/// `developer` launch the folders loaded there too, and load each module with the WebAssembly
/// engine, its compiled code cached under [`Paths::module_code_cache`]. A module whose component
/// does not load, or whose package no longer reads but whose kept manifest does, is handed over
/// unavailable with the reason; one without a readable manifest registers nothing. Without a data
/// root there is no store and nothing is read. Loading compiles or reads code and creates no
/// instance, and the packages' bytes are dropped once loaded.
pub fn installed_modules(paths: Option<&Paths>, developer: bool) -> InstalledModules {
    let engine = luxforge_wasm::WasmHost::new(paths.map(Paths::module_code_cache));
    let store = paths.map(|paths| ModuleStore::new(paths.module_store()));
    let launch = store
        .as_ref()
        .map(|store| store.launch(developer))
        .unwrap_or_default();
    let external = external_modules(&engine, &launch);
    InstalledModules {
        external,
        host: ModuleHost {
            store,
            engine: Some(Arc::new(Engine(engine))),
            launch: Arc::new(launch.record()),
        },
    }
}

/// The registry's external module for each module a launch read: loaded by `engine`, or
/// unavailable with the reason its package did not read when its manifest still does.
fn external_modules(engine: &luxforge_wasm::WasmHost, launch: &StoreLaunch) -> Vec<ExternalModule> {
    launch
        .modules
        .iter()
        .filter_map(|module| {
            let external = match &module.read {
                Ok(package) => engine.external(package).ok()?,
                Err(unread) => {
                    ExternalModule::unavailable(unread.manifest.as_ref()?, &unread.reason).ok()?
                }
            };
            Some(external.with_origin(module.origin))
        })
        .collect()
}

/// The engine as the module store's check: a component loads as a launch would load it, or the
/// package is refused with the engine's reason.
struct Engine(luxforge_wasm::WasmHost);

impl ModuleEngine for Engine {
    fn check(&self, package: &ModulePackage) -> Result<(), luxforge_core::Error> {
        self.0
            .load(package)
            .map(drop)
            .map_err(|error| PackageRefusal::Component.error(error.detail))
    }
}

/// A standalone version request has no catalog, network, Keychain or desktop side effects.
pub fn version_requested(mut args: impl Iterator<Item = std::ffi::OsString>) -> bool {
    args.next().as_deref() == Some(std::ffi::OsStr::new("--version")) && args.next().is_none()
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_a_lone_version_flag_is_a_version_request() {
        let requested = |args: &[&str]| super::version_requested(args.iter().map(Into::into));
        assert!(requested(&["--version"]));
        for args in [
            &[][..],
            &["--version", "--catalog"],
            &["--catalog", "--version"],
            &["-V"],
            &["version"],
        ] {
            assert!(!requested(args), "{args:?}");
        }
    }
}
