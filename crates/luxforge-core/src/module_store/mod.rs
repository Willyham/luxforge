//! Installed external modules (`docs/design/module-manager.md#lifecycle`): `modules.json`, the
//! per-version package store beside it under the data root, what a launch reads from them, and
//! each module's load state.
//!
//! - **The store.** `<data>/modules/modules.json` ([`STORE_FILE`]) is a format-marked document
//!   outside every catalog, written atomically under its lock through the one document store
//!   (`capabilities::document`). It lists the installed packages and, for developer mode, the
//!   folders loaded unpacked. Each installed package lives in `<data>/modules/packages/<id>/
//!   <version>-<hash>/` (the first twelve hex digits of its hash): the package's bytes exactly as
//!   they were validated, `package.lfmodule`, and a copy of its manifest, so a package that no
//!   longer reads still names its module. A file this build cannot read is refused by name and
//!   never rewritten. An evidence run's data root is inside its evidence directory, so it keeps
//!   its own store.
//! - **Next launch.** The registry is fixed for a launch, so a write changes only the store: the
//!   launch after it reads the new store. Until then [`statuses`] reports the module
//!   `pending-relaunch`. There is no rollback store: replacing a package removes the version it
//!   replaced, and uninstalling removes the package's directory.
//! - **Reading at launch.** [`ModuleStore::launch`] reads every installed package, and in a
//!   developer run every loaded folder, into memory before the registry is assembled, and records
//!   what it read ([`LaunchRecord`]). A package that no longer reads, or whose bytes are not the
//!   ones installed, is handed to the assembly with its kept manifest and the reason, so it
//!   registers unavailable; without a readable manifest its effect is simply unknown. A launch
//!   never fails because of the store: an unreadable `modules.json` loads no external module and
//!   says why.
//! - **Off the owner.** Reading, validating, copying and removing packages runs on the index
//!   lane's query thread, never the catalog owner's ([`crate::api`]'s module install handlers);
//!   the owner reads only `modules.json`, at most [`MAX_STORE_BYTES`], to answer a load state.
use crate::{
    Availability, Error, ModuleRegistry, atomic_file,
    capabilities::document::JsonDocument,
    module_layout::ModuleOrigin,
    module_package::{
        MANIFEST_FILE, MAX_MANIFEST_BYTES, ModuleEngine, ModuleManifest, ModulePackage,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
pub(crate) mod module_install_tests;

/// The store's document, in the store's root.
pub const STORE_FILE: &str = "modules.json";
/// The store document's format marker.
pub const STORE_FORMAT: u32 = 1;
/// The largest `modules.json`.
pub const MAX_STORE_BYTES: u64 = 64 * 1024;
/// The most modules the store holds, packages and folders together.
pub const MAX_STORED_MODULES: usize = 64;
/// The longest module identity the store keeps, in bytes.
pub const MAX_MODULE_ID: usize = 128;
/// The event every write to the store announces, under the request that made it.
pub const INSTALLED_EVENT: &str = "module.installed";
/// The directory under the store's root that holds the installed packages.
pub const PACKAGES_DIR: &str = "packages";
/// An installed package's bytes, in its version's directory.
pub const PACKAGE_FILE: &str = "package.lfmodule";
/// Where a package is written before it is renamed into place, under [`PACKAGES_DIR`].
const STAGING_DIR: &str = ".staging";
/// The lock every write holds across its document write and its package files, so two writers
/// never remove each other's directories.
const PACKAGES_LOCK: &str = "packages.lock";
/// The longest version, title or folder path the store keeps, in bytes.
const MAX_VERSION: usize = 64;
const MAX_TITLE: usize = 256;
const MAX_FOLDER_PATH: usize = 4096;

/// What `modules.json` holds: the installed packages and the folders loaded in developer mode,
/// each in the order it was first stored, which is the order a launch registers them in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredModules {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub packages: Vec<StoredPackage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<StoredFolder>,
}

/// One installed package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredPackage {
    /// The manifest's `id`.
    pub id: String,
    /// The manifest's `version`.
    pub version: String,
    /// The package file's hash, `sha256:` and 64 hex digits.
    pub hash: String,
    /// The payload format its effect declares.
    pub format: u32,
    /// Whether it ships a GPU program.
    pub program: bool,
    /// The manifest's `title`.
    pub title: String,
    /// When it was installed, in milliseconds since the Unix epoch.
    pub installed_ms: i64,
}

/// One folder a developer loaded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredFolder {
    /// The manifest's `id` when it was loaded.
    pub id: String,
    /// The folder's canonical absolute path.
    pub path: PathBuf,
    /// The manifest's `title` when it was loaded.
    pub title: String,
    /// When it was loaded, in milliseconds since the Unix epoch.
    pub loaded_ms: i64,
}

impl StoredPackage {
    /// The package's directory, relative to [`PACKAGES_DIR`]: `<id>/<version>-<first 12 hex>`.
    fn directory(&self) -> PathBuf {
        let digits = self.hash.strip_prefix("sha256:").unwrap_or(&self.hash);
        Path::new(&self.id).join(format!("{}-{}", self.version, &digits[..12]))
    }
}

impl StoredModules {
    fn count(&self) -> usize {
        self.packages.len() + self.folders.len()
    }

    /// The rules a stored document meets that its type cannot say, so a file that breaks one is
    /// refused rather than acted on: the store's size, distinct well-formed identities that are
    /// safe as a directory name, and versions, hashes, titles and paths of the shape a write
    /// stores.
    fn check(&self) -> Result<(), String> {
        let count = self.count();
        if count > MAX_STORED_MODULES {
            return Err(format!(
                "{count} modules; the store holds at most {MAX_STORED_MODULES}"
            ));
        }
        let mut ids = HashSet::new();
        for id in self
            .packages
            .iter()
            .map(|package| &package.id)
            .chain(self.folders.iter().map(|folder| &folder.id))
        {
            check_id(id)?;
            if !ids.insert(id) {
                return Err(format!("module {id} is stored twice"));
            }
        }
        for package in &self.packages {
            let id = &package.id;
            let version = &package.version;
            let safe = !version.is_empty()
                && version.len() <= MAX_VERSION
                && !version.starts_with('.')
                && version
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b".+-".contains(&byte));
            if !safe {
                return Err(format!("module {id} has the version {version:?}"));
            }
            let hex = package.hash.strip_prefix("sha256:").unwrap_or_default();
            if hex.len() != 64
                || !hex
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            {
                return Err(format!("module {id} has the hash {:?}", package.hash));
            }
            if package.title.len() > MAX_TITLE {
                return Err(format!(
                    "module {id}'s title is longer than {MAX_TITLE} bytes"
                ));
            }
        }
        for folder in &self.folders {
            let id = &folder.id;
            if !folder.path.is_absolute() || folder.path.as_os_str().len() > MAX_FOLDER_PATH {
                return Err(format!(
                    "module {id}'s folder {} is not an absolute path of at most {MAX_FOLDER_PATH} bytes",
                    folder.path.display()
                ));
            }
            if folder.title.len() > MAX_TITLE {
                return Err(format!(
                    "module {id}'s title is longer than {MAX_TITLE} bytes"
                ));
            }
        }
        Ok(())
    }
}

/// A stored identity: what a manifest's identity check admits, and safe as one directory name.
fn check_id(id: &str) -> Result<(), String> {
    let safe = !id.is_empty()
        && id.len() <= MAX_MODULE_ID
        && !id.starts_with('.')
        && !id.contains("..")
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_".contains(&byte));
    if safe {
        Ok(())
    } else {
        Err(format!("{id:?} is not a module identity"))
    }
}

/// The store under one root, normally `<data>/modules` (`luxforge_cli::Paths::module_store`). It
/// holds no state of its own: every call reads the document again, so two processes never act on a
/// stale copy. Nothing is created until the first install or folder load.
#[derive(Clone, Debug)]
pub struct ModuleStore {
    root: PathBuf,
    document: JsonDocument<StoredModules>,
}

/// What an install or folder load did, for its answer.
#[derive(Clone, Debug)]
pub(crate) struct Written<T> {
    /// What the store holds for the module now.
    pub entry: T,
    /// What it held for the module before, if anything.
    pub previous: Option<T>,
    /// Whether the store changed: false for a package or folder already stored as it is.
    pub changed: bool,
}

impl ModuleStore {
    /// The store whose document is `<root>/modules.json`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        Self {
            document: JsonDocument::new(root.clone(), STORE_FILE, MAX_STORE_BYTES, STORE_FORMAT)
                .checked(StoredModules::check),
            root,
        }
    }

    /// The store's root, which holds `modules.json` and [`PACKAGES_DIR`].
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// What the store holds now; empty when nothing was ever installed. A file this build cannot
    /// read is refused as `incompatible`, kept unchanged.
    pub fn read(&self) -> Result<StoredModules, Error> {
        Ok(self.document.read()?.unwrap_or_default())
    }

    /// The directory of an installed package.
    pub fn package_dir(&self, package: &StoredPackage) -> PathBuf {
        self.root.join(PACKAGES_DIR).join(package.directory())
    }

    /// Read what the store holds for a launch, as [`ModuleStore`] says: every installed package,
    /// and in a `developer` launch every loaded folder, in the store's order. Writes nothing.
    pub fn launch(&self, developer: bool) -> StoreLaunch {
        let stored = match self.read() {
            Ok(stored) => stored,
            Err(error) => {
                return StoreLaunch {
                    problem: Some(error.detail),
                    modules: Vec::new(),
                };
            }
        };
        let mut modules = Vec::with_capacity(stored.count());
        for entry in &stored.packages {
            let directory = self.package_dir(entry);
            let read = ModulePackage::read_file(&directory.join(PACKAGE_FILE))
                .and_then(|package| {
                    if package.hash == entry.hash {
                        Ok(package)
                    } else {
                        Err(Error::validation(format!(
                            "the stored package's bytes are not the ones installed: its hash is \
                             {}, not {}",
                            package.hash, entry.hash
                        )))
                    }
                })
                .and_then(|package| named(package, &entry.id))
                .map_err(|error| Unread {
                    manifest: kept_manifest(&directory, &entry.id),
                    reason: error.detail,
                });
            modules.push(LaunchModule {
                id: entry.id.clone(),
                origin: ModuleOrigin::Community,
                stored: Side::of_package(entry),
                read,
            });
        }
        if developer {
            for folder in &stored.folders {
                let read = ModulePackage::read_folder(&folder.path)
                    .and_then(|package| named(package, &folder.id))
                    .map_err(|error| Unread {
                        manifest: kept_manifest(&folder.path, &folder.id),
                        reason: error.detail,
                    });
                modules.push(LaunchModule {
                    id: folder.id.clone(),
                    origin: ModuleOrigin::Folder,
                    stored: Side::of_folder(folder),
                    read,
                });
            }
        }
        StoreLaunch {
            problem: None,
            modules,
        }
    }

    /// Install `package`, read from `bytes`, for the next launch: its bytes and manifest are
    /// written to a new version directory and renamed into place, then `modules.json` names it in
    /// place of any package with its identity, and the directory of the version it replaced is
    /// removed. A package already installed with the same hash changes nothing. A module loaded
    /// from a folder, a full store, or an action or effect another stored or built-in module
    /// provides is refused by name, and nothing is written.
    pub(crate) fn install(
        &self,
        bytes: &[u8],
        package: &ModulePackage,
        registry: &ModuleRegistry,
    ) -> Result<Written<StoredPackage>, Error> {
        let manifest = &package.manifest;
        let entry = StoredPackage {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            hash: package.hash.clone(),
            format: manifest.effect.format,
            program: package.program.is_some(),
            title: manifest.title.clone(),
            installed_ms: crate::editor::now_ms(),
        };
        let manifest_text = crate::modules::manifest_text(bytes)?;
        let _lock = atomic_file::lock(&self.root, PACKAGES_LOCK)?;
        let written = self.document.transact_published(|stored| {
            if let Some(folder) = stored.folders.iter().find(|folder| folder.id == entry.id) {
                return Err(Error::validation(format!(
                    "{} is loaded from the folder {} in developer mode; remove it with \
                     module.folder.remove before installing a package of it",
                    entry.id,
                    folder.path.display()
                )));
            }
            collisions(manifest, registry, stored)?;
            let at = stored
                .packages
                .iter()
                .position(|stored| stored.id == entry.id);
            let previous = at.map(|at| stored.packages[at].clone());
            if let Some(previous) = &previous
                && previous.hash == entry.hash
            {
                return Ok(Written {
                    entry: previous.clone(),
                    previous: Some(previous.clone()),
                    changed: false,
                });
            }
            if previous.is_none() && stored.count() >= MAX_STORED_MODULES {
                return Err(full());
            }
            self.place(&entry, bytes, &manifest_text)?;
            match at {
                Some(at) => stored.packages[at] = entry.clone(),
                None => stored.packages.push(entry.clone()),
            }
            Ok(Written {
                entry: entry.clone(),
                previous,
                changed: true,
            })
        })?;
        let written = written.finish()?;
        self.prune(&written.entry.id, Some(&written.entry));
        Ok(written)
    }

    /// Uninstall the package `id` for the next launch: `modules.json` no longer names it, and its
    /// directory is removed. The running launch keeps what it read. A module the store does not
    /// hold, or one loaded from a folder, is refused by name.
    pub(crate) fn uninstall(&self, id: &str) -> Result<StoredPackage, Error> {
        let _lock = atomic_file::lock(&self.root, PACKAGES_LOCK)?;
        let removed = self
            .document
            .transact_published(|stored| {
                if stored.folders.iter().any(|folder| folder.id == id) {
                    return Err(Error::validation(format!(
                        "{id} is loaded from a folder in developer mode; remove it with \
                         module.folder.remove"
                    )));
                }
                let at = stored
                    .packages
                    .iter()
                    .position(|package| package.id == id)
                    .ok_or_else(|| not_installed(id))?;
                Ok(stored.packages.remove(at))
            })?
            .finish()?;
        self.prune(id, None);
        Ok(removed)
    }

    /// Load the module in the folder `path` (canonical) for the next developer launch, read and
    /// checked as `package`. The store keeps only the path: every developer launch reads the
    /// folder again, so a rebuilt module loads at the next relaunch. The same folder again changes
    /// nothing; another folder of the same identity takes its place. An installed package of the
    /// identity, a full store or a collision is refused by name.
    pub(crate) fn load_folder(
        &self,
        path: &Path,
        package: &ModulePackage,
        registry: &ModuleRegistry,
    ) -> Result<Written<StoredFolder>, Error> {
        let manifest = &package.manifest;
        let entry = StoredFolder {
            id: manifest.id.clone(),
            path: path.to_path_buf(),
            title: manifest.title.clone(),
            loaded_ms: crate::editor::now_ms(),
        };
        let _lock = atomic_file::lock(&self.root, PACKAGES_LOCK)?;
        self.document
            .transact_published(|stored| {
                if stored.packages.iter().any(|stored| stored.id == entry.id) {
                    return Err(Error::validation(format!(
                        "{} is installed as a package; uninstall it with module.uninstall \
                         before loading it from a folder",
                        entry.id
                    )));
                }
                collisions(manifest, registry, stored)?;
                let at = stored
                    .folders
                    .iter()
                    .position(|folder| folder.id == entry.id);
                let previous = at.map(|at| stored.folders[at].clone());
                if let Some(previous) = &previous
                    && previous.path == entry.path
                {
                    return Ok(Written {
                        entry: previous.clone(),
                        previous: Some(previous.clone()),
                        changed: false,
                    });
                }
                if previous.is_none() && stored.count() >= MAX_STORED_MODULES {
                    return Err(full());
                }
                match at {
                    Some(at) => stored.folders[at] = entry.clone(),
                    None => stored.folders.push(entry.clone()),
                }
                Ok(Written {
                    entry: entry.clone(),
                    previous,
                    changed: true,
                })
            })?
            .finish()
    }

    /// Stop loading the folder module `id` from the next developer launch. The folder itself is
    /// never touched. A module the store does not hold as a folder is refused by name.
    pub(crate) fn remove_folder(&self, id: &str) -> Result<StoredFolder, Error> {
        let _lock = atomic_file::lock(&self.root, PACKAGES_LOCK)?;
        self.document
            .transact_published(|stored| {
                if stored.packages.iter().any(|package| package.id == id) {
                    return Err(Error::validation(format!(
                        "{id} is installed as a package, not loaded from a folder; uninstall it \
                         with module.uninstall"
                    )));
                }
                let at = stored
                    .folders
                    .iter()
                    .position(|folder| folder.id == id)
                    .ok_or_else(|| {
                        Error::validation(format!("no module {id} is loaded from a folder"))
                    })?;
                Ok(stored.folders.remove(at))
            })?
            .finish()
    }

    /// Write `entry`'s directory: the package's bytes and its manifest, staged under a fresh name
    /// and renamed into place, replacing a directory a failed write left there. A failure removes
    /// what it staged.
    fn place(&self, entry: &StoredPackage, bytes: &[u8], manifest: &str) -> Result<(), Error> {
        let packages = self.root.join(PACKAGES_DIR);
        let staging = packages
            .join(STAGING_DIR)
            .join(uuid::Uuid::new_v4().to_string());
        let target = self.package_dir(entry);
        let written = (|| -> std::io::Result<()> {
            fs::create_dir_all(&staging)?;
            atomic_file::stage(&staging.join(PACKAGE_FILE), bytes)?;
            atomic_file::stage(&staging.join(MANIFEST_FILE), manifest.as_bytes())?;
            atomic_file::sync_dir(&staging)?;
            let parent = target.parent().unwrap_or(&packages);
            fs::create_dir_all(parent)?;
            if target.exists() {
                fs::remove_dir_all(&target)?;
            }
            fs::rename(&staging, &target)?;
            atomic_file::sync_dir(parent)
        })();
        written.map_err(|error| {
            let _ = fs::remove_dir_all(&staging);
            atomic_file::file_error(target.display(), error.kind())
        })
    }

    /// Remove what no stored entry names: every version directory of `id` but `kept`'s, the
    /// module's own directory when none is kept, and what failed writes left staged. Only a writer
    /// holding the packages lock calls this. A removal that fails leaves only disposable files,
    /// which the next write of the module removes.
    fn prune(&self, id: &str, kept: Option<&StoredPackage>) {
        let packages = self.root.join(PACKAGES_DIR);
        let _ = fs::remove_dir_all(packages.join(STAGING_DIR));
        let module = packages.join(id);
        match kept {
            None => {
                let _ = fs::remove_dir_all(&module);
            }
            Some(kept) => {
                let kept = self.package_dir(kept);
                if let Ok(versions) = fs::read_dir(&module) {
                    for version in versions.flatten() {
                        if version.path() != kept {
                            let _ = fs::remove_dir_all(version.path());
                        }
                    }
                }
            }
        }
        let _ = atomic_file::sync_dir(&packages);
    }
}

fn full() -> Error {
    Error::resource_limit(format!(
        "the module store holds {MAX_STORED_MODULES} modules, its most; uninstall one first"
    ))
}

fn not_installed(id: &str) -> Error {
    Error::validation(format!("no module {id} is installed"))
}

/// `package` when its manifest names `id`, the identity the store holds it under.
fn named(package: ModulePackage, id: &str) -> Result<ModulePackage, Error> {
    if package.manifest.id == id {
        Ok(package)
    } else {
        Err(Error::validation(format!(
            "it now holds the module {}, not {id}",
            package.manifest.id
        )))
    }
}

/// The manifest kept in `directory` when it reads and names `id`.
fn kept_manifest(directory: &Path, id: &str) -> Option<ModuleManifest> {
    let bytes = atomic_file::read(&directory.join(MANIFEST_FILE), MAX_MANIFEST_BYTES).ok()??;
    let manifest = ModuleManifest::parse(std::str::from_utf8(&bytes).ok()?).ok()?;
    (manifest.id == id).then_some(manifest)
}

/// Refuse a module whose identity, action or effect a module the next launch also registers
/// already provides: one built into this build, or a stored external of another identity. A
/// stored external that this launch registered is found through the registry; one installed
/// since cannot be checked without reading its package, and the next launch's assembly refuses
/// whichever of two colliding externals comes second.
fn collisions(
    manifest: &ModuleManifest,
    registry: &ModuleRegistry,
    stored: &StoredModules,
) -> Result<(), Error> {
    let id = &manifest.id;
    let stays = |other: &str| match registry.external_origin(other) {
        None => true,
        Some(_) => {
            stored.packages.iter().any(|package| package.id == other)
                || stored.folders.iter().any(|folder| folder.id == other)
        }
    };
    if registry.module(id).is_some() && registry.external_origin(id).is_none() {
        return Err(collision(format!(
            "{id} is the identity of a module built into Luxforge"
        )));
    }
    let descriptor = manifest.descriptor();
    for action in &descriptor.actions {
        if let Some((provider, _)) = registry.action(&action.id) {
            let other = &provider.descriptor().id;
            if other != id && stays(other) {
                return Err(collision(format!(
                    "{id}'s action {} is already provided by {other}",
                    action.id
                )));
            }
        }
    }
    for effect in &descriptor.effects {
        if let Some(other) = registry.effect_module(&effect.id)
            && other != id
            && stays(other)
        {
            return Err(collision(format!(
                "{id}'s effect {} is already provided by {other}",
                effect.id
            )));
        }
    }
    Ok(())
}

/// Check `package` as a write would before anything is stored, and answer what the store holds
/// now: its identity, actions and effect must not collide ([`collisions`]), and the engine, when
/// the host has one, must load its component as a launch would (`PackageRefusal::Component`
/// otherwise). Runs off the owner: the engine compiles the component.
pub(crate) fn admit(
    host: &ModuleHost,
    registry: &ModuleRegistry,
    package: &ModulePackage,
) -> Result<StoredModules, Error> {
    let stored = match &host.store {
        Some(store) => store.read()?,
        None => StoredModules::default(),
    };
    collisions(&package.manifest, registry, &stored)?;
    if let Some(engine) = &host.engine {
        engine.check(package)?;
    }
    Ok(stored)
}

fn collision(detail: String) -> Error {
    Error::validation(detail).with_data(json!({"refusal": "collision"}))
}

/// What a launch read from the store, before the registry is assembled.
#[derive(Debug, Default)]
pub struct StoreLaunch {
    /// Why `modules.json` could not be read, when it could not; then nothing loads.
    pub problem: Option<String>,
    /// Every module the store names that this launch reads, in the store's order.
    pub modules: Vec<LaunchModule>,
}

/// One module a launch read from the store.
#[derive(Debug)]
pub struct LaunchModule {
    pub id: String,
    pub origin: ModuleOrigin,
    /// What the store says of it: for a package its version, hash, format and program; for a
    /// folder its path.
    pub stored: Side,
    /// Its package, or why it did not read.
    pub read: Result<ModulePackage, Unread>,
}

/// Why a stored module did not read, and its manifest when one was kept and still reads: with
/// one, the module registers unavailable with the reason; without, it registers nothing.
#[derive(Debug)]
pub struct Unread {
    pub manifest: Option<ModuleManifest>,
    pub reason: String,
}

impl StoreLaunch {
    /// What the launch read, kept for the launch's whole run once the packages are loaded and
    /// dropped: the catalog owner compares it with the store to answer each load state.
    pub fn record(&self) -> LaunchRecord {
        LaunchRecord {
            problem: self.problem.clone(),
            modules: self
                .modules
                .iter()
                .map(|module| Launched {
                    id: module.id.clone(),
                    origin: module.origin,
                    side: match &module.read {
                        Ok(package) => Side {
                            path: module.stored.path.clone(),
                            ..Side::of_read(package)
                        },
                        Err(_) => module.stored.clone(),
                    },
                    unread: module
                        .read
                        .as_ref()
                        .err()
                        .map(|unread| unread.reason.clone()),
                })
                .collect(),
        }
    }
}

/// What a launch read from the store ([`StoreLaunch::record`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LaunchRecord {
    /// Why `modules.json` could not be read at launch, when it could not.
    pub problem: Option<String>,
    pub modules: Vec<Launched>,
}

/// One module a launch read.
#[derive(Clone, Debug, PartialEq)]
pub struct Launched {
    pub id: String,
    pub origin: ModuleOrigin,
    pub side: Side,
    /// Why it did not read, when it did not.
    pub unread: Option<String>,
}

/// One side of a load state: what the next launch loads (`installed`) or what this launch read
/// (`running`). A package's side names its version, hash, payload format, GPU program and title; a
/// folder's its path, and once read the rest.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Side {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl Side {
    pub(crate) fn of_package(package: &StoredPackage) -> Self {
        Self {
            title: Some(package.title.clone()),
            version: Some(package.version.clone()),
            hash: Some(package.hash.clone()),
            format: Some(package.format),
            program: Some(package.program),
            path: None,
        }
    }

    fn of_folder(folder: &StoredFolder) -> Self {
        Self {
            title: Some(folder.title.clone()),
            path: Some(folder.path.clone()),
            ..Self::default()
        }
    }

    pub(crate) fn of_read(package: &ModulePackage) -> Self {
        Self {
            title: Some(package.manifest.title.clone()),
            version: Some(package.manifest.version.clone()),
            hash: Some(package.hash.clone()),
            format: Some(package.manifest.effect.format),
            program: Some(package.program.is_some()),
            path: None,
        }
    }

    /// Whether `self`, what the store names now, is what the launch read as `then`: the same
    /// package by hash, or the same folder by path.
    fn same(&self, origin: ModuleOrigin, then: &Self) -> bool {
        match origin {
            ModuleOrigin::Folder => self.path == then.path,
            _ => self.hash == then.hash,
        }
    }
}

/// The store, engine and launch record the catalog owner answers the lifecycle methods from,
/// given through [`crate::HostConfig::modules`]. The default has no store: every write is refused
/// `not-ready`, and no external module has a load state.
#[derive(Clone, Default)]
pub struct ModuleHost {
    /// The store, normally `<data>/modules`.
    pub store: Option<ModuleStore>,
    /// What checks a component as a launch loads it ([`ModuleEngine`]); without one, only the
    /// core's reader checks a package.
    pub engine: Option<Arc<dyn ModuleEngine>>,
    /// What this launch read from the store.
    pub launch: Arc<LaunchRecord>,
}

impl std::fmt::Debug for ModuleHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModuleHost")
            .field("store", &self.store.as_ref().map(ModuleStore::root))
            .field("engine", &self.engine.is_some())
            .field("launch", &self.launch)
            .finish()
    }
}

/// An external module's load state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LoadState {
    /// Registered and available: what the store names is what this launch loaded.
    Loaded,
    /// The store names something other than what this launch read, or nothing: an install, an
    /// update, an uninstall, a folder loaded or removed. It takes effect at the next launch.
    PendingRelaunch,
    /// This launch read it, and it did not load, or registered unavailable: `reason` says why.
    Failed,
}

/// The state of a module's GPU program. Until the program loader exists (TASK-007's validation
/// and first-use probe), this says only whether the package ships `program.wgsl`; every external
/// unit renders through the reference renderer meanwhile.
// Seam for the GPU program loader: replace `Present` with the program's own states (validated,
// probed and enabled, or disabled with its reason) from the launch's program set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum ProgramState {
    /// The package ships a program.
    Present,
    /// The package ships none: the module renders through the reference renderer.
    Absent,
}

/// One external module's load state, as `module.status` (`external`) and `module.package.list`
/// answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExternalStatus {
    pub module: String,
    pub origin: ModuleOrigin,
    pub state: LoadState,
    /// Why it failed, for a failed module.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What the next launch loads: the store's entry now; `null` once uninstalled or removed.
    pub installed: Option<Side>,
    /// What this launch read; `null` for a module stored since it started.
    pub running: Option<Side>,
    pub program: ProgramState,
}

/// What `modules.json` is now, as `module.package.list` answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum StoreState {
    /// It reads (or does not exist yet).
    Ready,
    /// It cannot be read: `reason` says why, and every write is refused until it is moved aside.
    Unreadable { reason: String },
    /// This host has no store, such as one without a data root.
    Unconfigured,
}

/// Every external module's load state, in the order this launch read them and then the order the
/// store holds those stored since, with what the store is now. Folder modules are listed only in a
/// developer run. Reads `modules.json` once; when it cannot be read, each module this launch read
/// is reported as the launch left it.
pub fn statuses(host: &ModuleHost, registry: &ModuleRegistry) -> (Vec<ExternalStatus>, StoreState) {
    let developer = registry.is_developer();
    let (now, state) = match &host.store {
        None => (None, StoreState::Unconfigured),
        Some(store) => match store.read() {
            Ok(stored) => (Some(stored), StoreState::Ready),
            Err(error) => (
                None,
                StoreState::Unreadable {
                    reason: error.detail,
                },
            ),
        },
    };
    let now: Option<Vec<(String, ModuleOrigin, Side)>> = now.map(|stored| {
        let packages = stored.packages.iter().map(|package| {
            (
                package.id.clone(),
                ModuleOrigin::Community,
                Side::of_package(package),
            )
        });
        let folders = stored.folders.iter().filter(|_| developer).map(|folder| {
            (
                folder.id.clone(),
                ModuleOrigin::Folder,
                Side::of_folder(folder),
            )
        });
        packages.chain(folders).collect()
    });
    let mut rows: Vec<ExternalStatus> = host
        .launch
        .modules
        .iter()
        .map(|then| {
            let current = match &now {
                // The store cannot be read: report what the launch read as it stands.
                None => Some((then.origin, then.side.clone())),
                Some(now) => now
                    .iter()
                    .find(|(id, _, _)| *id == then.id)
                    .map(|(_, origin, side)| (*origin, side.clone())),
            };
            let same = current.as_ref().is_some_and(|(origin, side)| {
                *origin == then.origin && side.same(*origin, &then.side)
            });
            let (state, reason) = if same {
                launch_state(registry, then)
            } else {
                (LoadState::PendingRelaunch, None)
            };
            let installed = current.map(|(_, side)| side);
            let program = program_state(Some(&then.side));
            ExternalStatus {
                module: then.id.clone(),
                origin: then.origin,
                state,
                reason,
                installed,
                running: Some(then.side.clone()),
                program,
            }
        })
        .collect();
    for (id, origin, side) in now.iter().flatten() {
        if rows.iter().any(|row| row.module == *id) {
            continue;
        }
        rows.push(ExternalStatus {
            module: id.clone(),
            origin: *origin,
            state: LoadState::PendingRelaunch,
            reason: None,
            program: program_state(Some(side)),
            installed: Some(side.clone()),
            running: None,
        });
    }
    (rows, state)
}

/// The load state of the external module `id`, when the store or this launch knows it.
pub fn status(host: &ModuleHost, registry: &ModuleRegistry, id: &str) -> Option<ExternalStatus> {
    statuses(host, registry)
        .0
        .into_iter()
        .find(|row| row.module == id)
}

/// How the launch left a module it read and the store still names.
fn launch_state(registry: &ModuleRegistry, then: &Launched) -> (LoadState, Option<String>) {
    if registry.external_origin(&then.id).is_some()
        && let Some(provider) = registry.module(&then.id)
    {
        return match &provider.descriptor().availability {
            Availability::Available => (LoadState::Loaded, None),
            Availability::Unavailable { reason } => (LoadState::Failed, Some(reason.clone())),
        };
    }
    let reason = registry
        .refused_externals()
        .iter()
        .find(|refused| refused.id == then.id)
        .map(|refused| refused.reason.clone())
        .or_else(|| then.unread.clone())
        .unwrap_or_else(|| "this launch did not register it".to_owned());
    (LoadState::Failed, Some(reason))
}

/// The GPU program's state from what a side says the package ships.
fn program_state(side: Option<&Side>) -> ProgramState {
    match side.and_then(|side| side.program) {
        Some(true) => ProgramState::Present,
        _ => ProgramState::Absent,
    }
}

/// `module.package.list`'s answer: every external module's load state, whether any waits for a
/// relaunch, and what the store is now.
pub(crate) fn listing(host: &ModuleHost, registry: &ModuleRegistry) -> Value {
    let (rows, store) = statuses(host, registry);
    let pending = rows
        .iter()
        .any(|row| row.state == LoadState::PendingRelaunch);
    json!({
        "modules": rows,
        "pending_relaunch": pending,
        "store": store,
        "launch_problem": host.launch.problem,
    })
}
