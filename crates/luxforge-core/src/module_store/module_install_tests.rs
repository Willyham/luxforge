//! The module store on its own: `modules.json`'s writes and refusals, the package directories, the
//! folders a developer loads, and what a launch reads back. The lifecycle methods through the
//! catalog owner, across a relaunch, are `api::owner::module_install_tests`.
use super::*;
use crate::module_package::{COMPONENT_FILE, PROGRAM_FILE, PackageRefusal};
use crate::{ErrorKind, ModuleRegistry};
use luxforge_testbase::paths::temp_dir;
use std::io::{Cursor, Write};
use zip::write::SimpleFileOptions;

/// The smallest component preamble the core's reader admits; no engine runs here.
pub(crate) const COMPONENT: &[u8] = b"\0asm\x0d\x00\x01\x00";

/// A one-field manifest of `id` at `version` whose actions are named `name` and whose effect
/// stores payload format `format`.
pub(crate) fn manifest(id: &str, name: &str, version: &str, format: u32) -> String {
    format!(
        r#"
api = "0.1"
id = "{id}"
name = "{name}"
title = "Glow"
hint = "Glow the picture"
version = "{version}"
authors = ["Example"]
license = "MIT"
repository = "https://example.org/glow"
toolset = "adjustments"

[effect]
stage = "color"
format = {format}

[[fields]]
name = "amount"
kind = "number"
min = -100
max = 100
default = 0
label = "Amount"

[[groups]]
label = "Glow"
fields = ["amount"]
"#
    )
}

/// A package of `manifest`, the component preamble and, with `program`, a GPU program.
pub(crate) fn package_bytes(manifest: &str, program: bool) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let mut entries = vec![
        (MANIFEST_FILE, manifest.as_bytes()),
        (COMPONENT_FILE, COMPONENT),
    ];
    let text = "fn lf_module(rgb: vec3<f32>) -> vec3<f32> { return rgb; }\n";
    if program {
        entries.push((PROGRAM_FILE, text.as_bytes()));
    }
    for (name, bytes) in entries {
        zip.start_file(name, SimpleFileOptions::default())
            .expect("an entry");
        zip.write_all(bytes).expect("its bytes");
    }
    zip.finish().expect("the archive").into_inner()
}

const GLOW: &str = "org.example.glow";

fn glow(version: &str) -> (Vec<u8>, ModulePackage) {
    let bytes = package_bytes(&manifest(GLOW, "glow", version, 1), true);
    let package = ModulePackage::read(&bytes).expect("the package reads");
    (bytes, package)
}

struct Store {
    dir: PathBuf,
    store: ModuleStore,
    registry: ModuleRegistry,
}

impl Store {
    fn new(name: &str) -> Self {
        let dir = temp_dir(&format!("module-install-store-{name}"));
        Self {
            store: ModuleStore::new(dir.join("modules")),
            dir,
            registry: ModuleRegistry::builtin(),
        }
    }

    fn document(&self) -> Value {
        serde_json::from_slice(&fs::read(self.store.root().join(STORE_FILE)).unwrap()).unwrap()
    }

    /// Every directory under the packages' root, relative to it, sorted.
    fn directories(&self) -> Vec<String> {
        let root = self.store.root().join(PACKAGES_DIR);
        let mut found = Vec::new();
        for module in fs::read_dir(&root).into_iter().flatten().flatten() {
            for version in fs::read_dir(module.path()).into_iter().flatten().flatten() {
                found.push(
                    version
                        .path()
                        .strip_prefix(&root)
                        .unwrap()
                        .display()
                        .to_string(),
                );
            }
        }
        found.sort();
        found
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// An install stores exactly the bytes it validated and a copy of the manifest under the
/// package's version directory, records the package in a format-marked `modules.json`, and a
/// launch reads the same package back.
#[test]
fn module_install_stores_the_validated_bytes_and_a_launch_reads_them_back() {
    let fixture = Store::new("stores");
    let (bytes, package) = glow("1.0.0");
    let written = fixture
        .store
        .install(&bytes, &package, &fixture.registry)
        .unwrap();
    assert!(written.changed && written.previous.is_none());
    let document = fixture.document();
    assert_eq!(document["format"], STORE_FORMAT);
    let stored = &document["packages"][0];
    assert_eq!(
        (&stored["id"], &stored["version"], &stored["hash"]),
        (&json!(GLOW), &json!("1.0.0"), &json!(package.hash))
    );
    assert_eq!(
        (&stored["format"], &stored["program"]),
        (&json!(1), &json!(true))
    );
    let directory = fixture.store.package_dir(&written.entry);
    assert_eq!(fs::read(directory.join(PACKAGE_FILE)).unwrap(), bytes);
    assert!(directory.join(MANIFEST_FILE).is_file());
    let hex = &package.hash["sha256:".len()..];
    assert_eq!(
        fixture.directories(),
        [format!("{GLOW}/1.0.0-{}", &hex[..12])]
    );

    let launch = fixture.store.launch(false);
    assert_eq!(launch.problem, None);
    assert_eq!(launch.modules.len(), 1);
    let read = launch.modules[0].read.as_ref().expect("the package reads");
    assert_eq!(read, &package);
    let record = launch.record();
    assert_eq!(record.modules[0].origin, ModuleOrigin::Community);
    assert_eq!(
        record.modules[0].side.hash.as_deref(),
        Some(package.hash.as_str())
    );
    assert_eq!(record.modules[0].unread, None);
}

/// The same package again changes nothing; another version replaces it, and the version it
/// replaced is removed, since there is no rollback store.
#[test]
fn module_install_of_the_same_package_is_unchanged_and_a_new_version_replaces_the_old() {
    let fixture = Store::new("replace");
    let (bytes, package) = glow("1.0.0");
    fixture
        .store
        .install(&bytes, &package, &fixture.registry)
        .unwrap();
    let before = fs::read(fixture.store.root().join(STORE_FILE)).unwrap();
    let again = fixture
        .store
        .install(&bytes, &package, &fixture.registry)
        .unwrap();
    assert!(!again.changed);
    assert_eq!(
        fs::read(fixture.store.root().join(STORE_FILE)).unwrap(),
        before,
        "an unchanged install writes nothing"
    );

    let (newer_bytes, newer) = glow("1.1.0");
    let replaced = fixture
        .store
        .install(&newer_bytes, &newer, &fixture.registry)
        .unwrap();
    assert!(replaced.changed);
    assert_eq!(replaced.previous.unwrap().version, "1.0.0");
    assert_eq!(fixture.document()["packages"].as_array().unwrap().len(), 1);
    assert_eq!(fixture.document()["packages"][0]["version"], "1.1.0");
    let directories = fixture.directories();
    assert_eq!(directories.len(), 1, "{directories:?}");
    assert!(directories[0].starts_with(&format!("{GLOW}/1.1.0-")));
}

/// Uninstalling removes the entry and the package's directory; an id the store does not hold is
/// refused by name and writes nothing.
#[test]
fn module_install_uninstall_removes_the_package_and_refuses_an_unknown_id() {
    let fixture = Store::new("uninstall");
    let (bytes, package) = glow("1.0.0");
    fixture
        .store
        .install(&bytes, &package, &fixture.registry)
        .unwrap();
    let removed = fixture.store.uninstall(GLOW).unwrap();
    assert_eq!(removed.hash, package.hash);
    assert_eq!(fixture.document().get("packages"), None);
    assert!(!fixture.store.root().join(PACKAGES_DIR).join(GLOW).exists());
    let refused = fixture.store.uninstall(GLOW).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Validation);
    assert!(refused.detail.contains(GLOW), "{}", refused.detail);
    assert!(fixture.store.launch(false).modules.is_empty());
}

/// A `modules.json` of another format, or one that breaks a rule, is refused by every write and
/// kept byte for byte; a launch reads nothing from it and says why.
#[test]
fn module_install_refuses_an_unreadable_store_without_rewriting_it() {
    let fixture = Store::new("unreadable");
    fs::create_dir_all(fixture.store.root()).unwrap();
    let (bytes, package) = glow("1.0.0");
    for document in [
        br#"{"format":2,"packages":[]}"#.as_slice(),
        br#"{"format":1,"packages":[],"extra":true}"#,
        br#"{"format":1,"folders":[{"id":"../escape","path":"/x","title":"","loaded_ms":0}]}"#,
        b"not json",
    ] {
        let path = fixture.store.root().join(STORE_FILE);
        fs::write(&path, document).unwrap();
        let refused = fixture
            .store
            .install(&bytes, &package, &fixture.registry)
            .unwrap_err();
        assert_eq!(refused.kind, ErrorKind::Incompatible, "{refused:?}");
        assert!(
            refused.detail.contains("kept unchanged"),
            "{}",
            refused.detail
        );
        assert_eq!(
            fixture.store.uninstall(GLOW).unwrap_err().kind,
            ErrorKind::Incompatible
        );
        assert_eq!(fs::read(&path).unwrap(), document, "the file is kept");
        let launch = fixture.store.launch(true);
        assert!(launch.modules.is_empty());
        assert!(launch.problem.is_some());
    }
    assert!(fixture.directories().is_empty(), "nothing was placed");
}

/// A stored package that no longer reads, or whose bytes changed, is handed to the launch with
/// its kept manifest and the reason, so it registers unavailable; without the manifest it
/// registers nothing.
#[test]
fn module_install_keeps_the_manifest_of_a_package_that_no_longer_reads() {
    let fixture = Store::new("broken");
    let (bytes, package) = glow("1.0.0");
    let written = fixture
        .store
        .install(&bytes, &package, &fixture.registry)
        .unwrap();
    let directory = fixture.store.package_dir(&written.entry);
    let mut changed = bytes.clone();
    let last = changed.len() - 1;
    changed[last] ^= 0xff;
    fs::write(directory.join(PACKAGE_FILE), &changed).unwrap();
    let launch = fixture.store.launch(false);
    let unread = launch.modules[0].read.as_ref().unwrap_err();
    assert_eq!(
        unread
            .manifest
            .as_ref()
            .map(|manifest| manifest.id.as_str()),
        Some(GLOW)
    );
    assert!(!unread.reason.is_empty());
    assert_eq!(
        launch.record().modules[0].unread.as_deref(),
        Some(unread.reason.as_str())
    );
    assert_eq!(
        launch.record().modules[0].side.version.as_deref(),
        Some("1.0.0")
    );

    // Bytes that still read but are not the ones installed are refused by their hash.
    let (other, _) = glow("1.0.1");
    fs::write(directory.join(PACKAGE_FILE), &other).unwrap();
    let launch = fixture.store.launch(false);
    let unread = launch.modules[0].read.as_ref().unwrap_err();
    assert!(
        unread.reason.contains("not the ones installed"),
        "{}",
        unread.reason
    );

    fs::remove_file(directory.join(MANIFEST_FILE)).unwrap();
    fs::remove_file(directory.join(PACKAGE_FILE)).unwrap();
    let launch = fixture.store.launch(false);
    assert!(
        launch.modules[0]
            .read
            .as_ref()
            .unwrap_err()
            .manifest
            .is_none()
    );
}

/// A folder is stored by its path and read only by a developer launch; a package and a folder of
/// one identity are refused both ways; removing the folder leaves it untouched; a rebuilt folder
/// hashes anew.
#[test]
fn module_install_folders_load_only_in_developer_launches() {
    let fixture = Store::new("folders");
    let folder = fixture.dir.join("glow-module");
    fs::create_dir_all(&folder).unwrap();
    fs::write(
        folder.join(MANIFEST_FILE),
        manifest(GLOW, "glow", "0.1.0", 1),
    )
    .unwrap();
    fs::write(folder.join(COMPONENT_FILE), COMPONENT).unwrap();
    fs::write(folder.join("README.md"), "sources and notes are not read").unwrap();
    let package = ModulePackage::read_folder(&folder).unwrap();
    assert_eq!(package.program, None);
    let written = fixture
        .store
        .load_folder(&folder, &package, &fixture.registry)
        .unwrap();
    assert!(written.changed);
    assert!(
        !fixture
            .store
            .load_folder(&folder, &package, &fixture.registry)
            .unwrap()
            .changed
    );
    assert!(
        fixture.store.launch(false).modules.is_empty(),
        "not outside developer mode"
    );
    let launch = fixture.store.launch(true);
    assert_eq!(launch.modules[0].origin, ModuleOrigin::Folder);
    assert_eq!(launch.modules[0].read.as_ref().unwrap().hash, package.hash);

    let (bytes, installed) = glow("1.0.0");
    let refused = fixture
        .store
        .install(&bytes, &installed, &fixture.registry)
        .unwrap_err();
    assert!(
        refused.detail.contains("module.folder.remove"),
        "{}",
        refused.detail
    );
    assert!(
        fixture
            .store
            .uninstall(GLOW)
            .unwrap_err()
            .detail
            .contains("module.folder.remove")
    );

    fs::write(folder.join(PROGRAM_FILE), "fn lf_module() {}\n").unwrap();
    let rebuilt = ModulePackage::read_folder(&folder).unwrap();
    assert_ne!(rebuilt.hash, package.hash, "a changed folder hashes anew");

    let removed = fixture.store.remove_folder(GLOW).unwrap();
    assert_eq!(removed.path, folder);
    assert!(
        folder.join(COMPONENT_FILE).is_file(),
        "the folder is untouched"
    );
    assert!(fixture.store.launch(true).modules.is_empty());
    assert_eq!(
        fixture.store.remove_folder(GLOW).unwrap_err().kind,
        ErrorKind::Validation
    );
    fixture
        .store
        .install(&bytes, &installed, &fixture.registry)
        .unwrap();
    let refused = fixture
        .store
        .load_folder(&folder, &package, &fixture.registry)
        .unwrap_err();
    assert!(
        refused.detail.contains("module.uninstall"),
        "{}",
        refused.detail
    );
}

/// A folder missing a file, or with a file of the wrong kind, is refused by its reason.
#[test]
fn module_install_refuses_a_folder_without_its_files() {
    let fixture = Store::new("folder-refusals");
    let folder = fixture.dir.join("empty-module");
    fs::create_dir_all(&folder).unwrap();
    let refusal =
        |folder: &Path| PackageRefusal::of(&ModulePackage::read_folder(folder).unwrap_err());
    assert_eq!(refusal(&folder), Some(PackageRefusal::MissingFile));
    fs::write(
        folder.join(MANIFEST_FILE),
        manifest(GLOW, "glow", "0.1.0", 1),
    )
    .unwrap();
    assert_eq!(refusal(&folder), Some(PackageRefusal::MissingFile));
    fs::create_dir_all(folder.join(COMPONENT_FILE)).unwrap();
    assert_eq!(refusal(&folder), Some(PackageRefusal::UnexpectedEntry));
    fs::remove_dir(folder.join(COMPONENT_FILE)).unwrap();
    fs::write(folder.join(COMPONENT_FILE), b"\0asm\x01\x00\x00\x00").unwrap();
    assert_eq!(refusal(&folder), Some(PackageRefusal::Content));
}

/// A module whose action or effect a built-in module provides is refused by name before anything
/// is written, as is one past the store's limit.
#[test]
fn module_install_refuses_a_collision_and_a_full_store() {
    let fixture = Store::new("collisions");
    let bytes = package_bytes(
        &manifest("org.example.shade", "vignette", "1.0.0", 1),
        false,
    );
    let package = ModulePackage::read(&bytes).unwrap();
    let refused = fixture
        .store
        .install(&bytes, &package, &fixture.registry)
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Validation);
    assert!(
        refused.detail.contains("set-vignette") && refused.detail.contains("luxforge.vignette"),
        "{}",
        refused.detail
    );
    assert_eq!(refused.data.as_deref().unwrap()["refusal"], "collision");
    assert!(!fixture.store.root().join(STORE_FILE).exists());

    for at in 0..MAX_STORED_MODULES {
        let id = format!("org.example.m{at}");
        let bytes = package_bytes(&manifest(&id, &format!("m{at}"), "1.0.0", 1), false);
        let package = ModulePackage::read(&bytes).unwrap();
        fixture
            .store
            .install(&bytes, &package, &fixture.registry)
            .unwrap();
    }
    let (bytes, package) = glow("1.0.0");
    let refused = fixture
        .store
        .install(&bytes, &package, &fixture.registry)
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::ResourceLimit, "{refused:?}");
}
