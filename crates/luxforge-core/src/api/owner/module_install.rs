//! The installed external modules' lifecycle methods (`docs/design/module-manager.md#lifecycle`):
//! `module.package.inspect`, `module.install`, `module.uninstall`, `module.package.list`, the
//! developer-only `module.folder.load` and `module.folder.remove`, and the `external` load state
//! `module.status` adds for an external module.
//!
//! Everything that reads, hashes, validates, compiles, copies or removes a package runs on the
//! index lane's query thread ([`files::ask`]), so the catalog owner never touches a package: the
//! call waits parked, and the owner only announces the write and answers. Every write takes
//! effect at the next launch, says so in its answer's `status` (`pending-relaunch`) and announces
//! one `module.installed` event under its request; a write that changed nothing announces none.
//! Installing is the caller's explicit call and needs no grant: a phase-2 module holds no
//! capabilities.
use super::{Call, Owner, files};
use crate::{
    Error, ModuleRegistry,
    api::{Origin, announce_once, params::host_params},
    capabilities::host::ModuleParams,
    module_layout::{ModuleOrigin, placements},
    module_package::{ModulePackage, read_package_file},
    module_store::{self, INSTALLED_EVENT, MAX_MODULE_ID, ModuleHost, ModuleStore, Side},
};
use serde_json::{Value, json};
use std::{path::Path, path::PathBuf, sync::Arc};

host_params! {
    /// `module.package.inspect` and `module.install`.
    pub(in crate::api) struct PackagePath {
        path: PathBuf = path().notes("an absolute path to a .lfmodule package file"),
    }
}

host_params! {
    /// `module.folder.load`.
    pub(in crate::api) struct FolderPath {
        path: PathBuf = path().notes("an absolute path to an unpacked module folder holding luxforge-module.toml, module.wasm and optionally program.wgsl at its root"),
    }
}

host_params! {
    /// `module.uninstall` and `module.folder.remove`.
    pub(in crate::api) struct StoredModule {
        module: String = string(MAX_MODULE_ID).notes("the module's id, as module.package.list answers it"),
    }
}

fn absolute(path: &Path) -> Result<(), Error> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(Error::validation(format!(
            "{} is not an absolute path",
            path.display()
        )))
    }
}

/// What a write needs off the owner: the module host, with a store, and the registry.
fn writer(owner: &Owner) -> Result<(ModuleHost, ModuleStore, Arc<ModuleRegistry>), Error> {
    let host = owner.host.modules().clone();
    let store = host
        .store
        .clone()
        .ok_or_else(|| Error::not_ready("no module store is configured; pass --data-root"))?;
    Ok((host, store, owner.service.registry().clone()))
}

/// Announce `module.installed` under `call`'s request, on the owner, when the write changed the
/// store.
fn announcer(call: &Call<'_>) -> Origin {
    Origin::new(INSTALLED_EVENT, &call.origin.request_id)
}

/// Read `path`'s package and check it as a write would ([`module_store::admit`]), off the owner.
fn read_admitted(
    host: &ModuleHost,
    registry: &ModuleRegistry,
    path: &Path,
) -> Result<(Vec<u8>, ModulePackage, module_store::StoredModules), Error> {
    let bytes = read_package_file(path)?;
    let package = ModulePackage::read(&bytes)?;
    let stored = module_store::admit(host, registry, &package)?;
    Ok((bytes, package, stored))
}

/// The notice an answer carries when the package's payload format differs from the installed
/// version's: existing edits are unavailable until a version that reads them is installed.
fn format_change(id: &str, installed: Option<u32>, package: u32) -> Value {
    match installed {
        Some(installed) if installed != package => json!({
            "installed": installed,
            "package": package,
            "notice": format!(
                "this version of {id} stores payload format {package}, and the installed version \
                 format {installed}: photos with {id} layers of format {installed} will be \
                 unavailable until a version that reads them is installed"
            ),
        }),
        _ => Value::Null,
    }
}

/// `module.package.inspect`: the package at `path`, read and checked without installing it, for
/// the review sheet.
pub(in crate::api) fn inspect(
    owner: &mut Owner,
    _: &Call<'_>,
    params: PackagePath,
) -> Result<Value, Error> {
    absolute(&params.path)?;
    let host = owner.host.modules().clone();
    let registry = owner.service.registry().clone();
    files::ask(owner, move || {
        let answer = inspected(&host, &registry, &params.path);
        Box::new(move |_| answer)
    })
}

fn inspected(host: &ModuleHost, registry: &ModuleRegistry, path: &Path) -> Result<Value, Error> {
    let (bytes, package, stored) = read_admitted(host, registry, path)?;
    let manifest = &package.manifest;
    let descriptor = manifest.descriptor();
    let installed = stored
        .packages
        .iter()
        .find(|stored| stored.id == manifest.id)
        .map(|stored| (ModuleOrigin::Community, Side::of_package(stored)))
        .or_else(|| {
            stored
                .folders
                .iter()
                .find(|folder| folder.id == manifest.id)
                .map(|folder| {
                    let side = Side {
                        title: Some(folder.title.clone()),
                        path: Some(folder.path.clone()),
                        ..Side::default()
                    };
                    (ModuleOrigin::Folder, side)
                })
        });
    let installed_format = installed.as_ref().and_then(|(_, side)| side.format);
    Ok(json!({
        "path": path,
        "bytes": bytes.len(),
        "hash": package.hash,
        "manifest": manifest,
        "program": package.program.is_some(),
        "descriptor": descriptor,
        "placements": placements(descriptor),
        "methods": descriptor
            .actions
            .iter()
            .map(|action| format!("edit.{}", action.id))
            .collect::<Vec<_>>(),
        "access": {"network": "none", "files": "none"},
        "reviewed": false,
        "installed": installed.map(|(origin, side)| {
            let mut installed = json!(side);
            installed["origin"] = json!(origin);
            installed
        }),
        "already_installed": stored
            .packages
            .iter()
            .any(|stored| stored.id == manifest.id && stored.hash == package.hash),
        "format_change": format_change(&manifest.id, installed_format, manifest.effect.format),
        "status": module_store::status(host, registry, &manifest.id),
    }))
}

/// `module.install`: copy the package at `path` into the store for the next launch.
pub(in crate::api) fn install(
    owner: &mut Owner,
    call: &Call<'_>,
    params: PackagePath,
) -> Result<Value, Error> {
    absolute(&params.path)?;
    let (host, store, registry) = writer(owner)?;
    let origin = announcer(call);
    files::ask(owner, move || {
        let answer = installed(&host, &store, &registry, &params.path);
        Box::new(move |owner| {
            let (answer, changed) = answer?;
            if changed {
                announce_once(&mut owner.announced, &origin);
            }
            Ok(answer)
        })
    })
}

fn installed(
    host: &ModuleHost,
    store: &ModuleStore,
    registry: &ModuleRegistry,
    path: &Path,
) -> Result<(Value, bool), Error> {
    let (bytes, package, _) = read_admitted(host, registry, path)?;
    let written = store.install(&bytes, &package, registry)?;
    let entry = &written.entry;
    let outcome = match (&written.previous, written.changed) {
        (_, false) => "unchanged",
        (Some(_), true) => "replaced",
        (None, true) => "installed",
    };
    let previous = written.previous.as_ref().filter(|_| written.changed);
    Ok((
        json!({
            "module": entry.id,
            "origin": ModuleOrigin::Community,
            "outcome": outcome,
            "installed": Side::of_package(entry),
            "previous": previous.map(Side::of_package),
            "format_change": format_change(
                &entry.id,
                previous.map(|previous| previous.format),
                entry.format,
            ),
            "status": module_store::status(host, registry, &entry.id),
        }),
        written.changed,
    ))
}

/// `module.uninstall`: remove an installed package at the next launch.
pub(in crate::api) fn uninstall(
    owner: &mut Owner,
    call: &Call<'_>,
    params: StoredModule,
) -> Result<Value, Error> {
    let (host, store, registry) = writer(owner)?;
    let origin = announcer(call);
    files::ask(owner, move || {
        let answer = store.uninstall(&params.module).map(|removed| {
            json!({
                "module": removed.id,
                "removed": Side::of_package(&removed),
                "status": module_store::status(&host, &registry, &removed.id),
            })
        });
        Box::new(move |owner| {
            let answer = answer?;
            announce_once(&mut owner.announced, &origin);
            Ok(answer)
        })
    })
}

/// `module.folder.load` (developer mode): load the module in a folder at the next launch.
pub(in crate::api) fn folder_load(
    owner: &mut Owner,
    call: &Call<'_>,
    params: FolderPath,
) -> Result<Value, Error> {
    absolute(&params.path)?;
    let (host, store, registry) = writer(owner)?;
    let origin = announcer(call);
    files::ask(owner, move || {
        let answer = (|| -> Result<(Value, bool), Error> {
            let path = params.path.canonicalize().map_err(|error| {
                Error::file_access(format!("cannot read {}: {error}", params.path.display()))
            })?;
            let package = ModulePackage::read_folder(&path)?;
            module_store::admit(&host, &registry, &package)?;
            let written = store.load_folder(&path, &package, &registry)?;
            let entry = &written.entry;
            let answer = json!({
                "module": entry.id,
                "origin": ModuleOrigin::Folder,
                "outcome": match (&written.previous, written.changed) {
                    (_, false) => "unchanged",
                    (Some(_), true) => "replaced",
                    (None, true) => "loaded",
                },
                "path": entry.path,
                "read": Side::of_read(&package),
                "previous": written.previous.as_ref().filter(|_| written.changed).map(|previous| &previous.path),
                "status": module_store::status(&host, &registry, &entry.id),
            });
            Ok((answer, written.changed))
        })();
        Box::new(move |owner| {
            let (answer, changed) = answer?;
            if changed {
                announce_once(&mut owner.announced, &origin);
            }
            Ok(answer)
        })
    })
}

/// `module.folder.remove` (developer mode): stop loading a folder module at the next launch.
pub(in crate::api) fn folder_remove(
    owner: &mut Owner,
    call: &Call<'_>,
    params: StoredModule,
) -> Result<Value, Error> {
    let (host, store, registry) = writer(owner)?;
    let origin = announcer(call);
    files::ask(owner, move || {
        let answer = store.remove_folder(&params.module).map(|removed| {
            json!({
                "module": removed.id,
                "path": removed.path,
                "status": module_store::status(&host, &registry, &removed.id),
            })
        });
        Box::new(move |owner| {
            let answer = answer?;
            announce_once(&mut owner.announced, &origin);
            Ok(answer)
        })
    })
}

/// `module.package.list`: every external module's load state, read from `modules.json` on the
/// owner (at most `MAX_STORE_BYTES`).
pub(in crate::api) fn list(
    owner: &mut Owner,
    _: &Call<'_>,
    _: crate::api::params::NoParams,
) -> Result<Value, Error> {
    Ok(module_store::listing(
        owner.host.modules(),
        owner.service.registry(),
    ))
}

/// `module.status`: the capability host's status of a registered module, with `external`, the
/// load state, for an external one. An id only the store knows, such as a package installed since
/// launch, answers its load state alone.
pub(in crate::api) fn status(
    owner: &mut Owner,
    _: &Call<'_>,
    params: ModuleParams,
) -> Result<Value, Error> {
    let registry = owner.service.registry().clone();
    let external = module_store::status(owner.host.modules(), &registry, &params.module_id);
    if registry.module(&params.module_id).is_none()
        && let Some(external) = &external
    {
        return Ok(json!({"module_id": params.module_id, "external": external}));
    }
    let mut status = owner.host.status(&owner.jobs, &registry, params)?;
    if let Some(external) = external {
        status["external"] = json!(external);
    }
    Ok(status)
}
