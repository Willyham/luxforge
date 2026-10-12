//! Launching past an unsupported catalog, against real catalogs: a remembered catalog another
//! version made opens the sheet; Start a new catalog reaches a working editor over a new,
//! remembered catalog beside it; Quit creates and writes nothing; and either way the refused
//! catalog, its write-ahead log, its artifacts and its index keep every byte. An explicit
//! `--catalog` and another instance's catalog keep the command line's refusal.
use super::{
    lifecycle::{self, Prepared},
    message::{Message, unsupported_catalog::UnsupportedCatalogMessage as Sheet},
    tasks::call,
    unsupported_catalog::{Next, Shell, Unsupported, start_new},
};
use crate::{
    Config,
    state::{
        preferences::{CatalogOverride, LaunchCatalog},
        unsupported_catalog::{Phase, Refusal},
    },
};
use luxforge_core::{ErrorKind, HostConfig, ModuleRegistry};
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// A catalog this build made, with a photograph, then marked `format` as a later version would
/// and left as a stopped one leaves it: the marker and a row in its write-ahead log, never
/// checkpointed, and an artifacts and an index directory beside it.
fn seed(folder: &Path, format: i64) -> PathBuf {
    let catalog = folder.join("catalog.sqlite");
    let mut service = luxforge_core::EditorService::open(&catalog).unwrap();
    service
        .import(&luxforge_testbase::paths::jpeg())
        .expect("a photograph in the catalog");
    drop(service);
    let wal = luxforge_testbase::paths::wal(&catalog);
    let connection = rusqlite::Connection::open(&catalog).unwrap();
    connection
        .execute_batch(&format!(
            "PRAGMA journal_mode=WAL;
             PRAGMA wal_autocheckpoint=0;
             CREATE TABLE later_version(value INTEGER);
             INSERT INTO later_version VALUES (1);
             PRAGMA user_version={format};"
        ))
        .unwrap();
    // The files as they stand while it is open, put back once closing has folded the log in.
    let (bytes, log) = (
        std::fs::read(&catalog).unwrap(),
        std::fs::read(&wal).unwrap(),
    );
    drop(connection);
    std::fs::write(&catalog, bytes).unwrap();
    std::fs::write(&wal, log).unwrap();
    for (dir, file) in [
        ("catalog.artifacts", "manifest.json"),
        ("catalog.index", "index.sqlite"),
    ] {
        let dir = folder.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), format!("{dir:?} of format {format}")).unwrap();
    }
    catalog
}

/// Every file under `folder` that belongs to the refused catalog, with its bytes.
fn refused_files(folder: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, files);
            } else {
                files.insert(path.clone(), std::fs::read(&path).unwrap());
            }
        }
    }
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(folder).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name == "catalog.sqlite" || name == "catalog.sqlite-wal" {
            files.insert(path.clone(), std::fs::read(&path).unwrap());
        } else if name == "catalog.artifacts" || name == "catalog.index" {
            walk(&path, &mut files);
        }
    }
    files
}

/// A launch over `root`: its preferences under `root/config`, its catalog `catalog`, chosen as
/// `forced` says, and the default `root/config/catalog.sqlite`.
fn prepared(root: &Path, catalog: &Path, forced: Option<CatalogOverride>) -> Prepared {
    Prepared {
        config: Config {
            launch_catalog: Some(LaunchCatalog {
                path: catalog.to_path_buf(),
                default: Some(root.join("config").join("catalog.sqlite")),
                forced,
                missing: None,
            }),
            ..Config::default()
        },
        registry: Arc::new(ModuleRegistry::developer()),
        host: HostConfig {
            preferences_dir: Some(root.join("config")),
            ..HostConfig::unconfigured()
        },
        window: (1440.0, 900.0),
    }
}

/// What the launch does with `catalog`: the sheet it opens, or the command line's message.
fn launch(prepared: Prepared, catalog: &Path) -> Result<Unsupported, String> {
    match lifecycle::own(&prepared, catalog) {
        Ok(_) => panic!("the catalog was opened"),
        Err(error) => {
            let refusal = prepared
                .config
                .launch_catalog
                .as_ref()
                .and_then(|launch| Refusal::offered(launch, &error));
            match refusal {
                Some(refusal) => {
                    let (sheet, _) =
                        Unsupported::new(refusal, prepared, Arc::new(Mutex::new(None)));
                    Ok(sheet)
                }
                None => Err(lifecycle::refused(catalog, &error)),
            }
        }
    }
}

fn stored(root: &Path) -> Option<serde_json::Value> {
    std::fs::read_to_string(root.join("config").join("preferences.json"))
        .ok()
        .map(|text| serde_json::from_str(&text).unwrap())
}

/// A remembered catalog another version made opens the sheet naming both formats; Start a new
/// catalog creates `catalog (format 1).sqlite` beside it, remembers it, and the window becomes a
/// working editor over it; the refused catalog, its log, artifacts and index keep every byte.
#[test]
fn unsupported_catalog_start_opens_and_remembers_a_new_catalog_and_keeps_the_refused_one() {
    let root = luxforge_testbase::paths::temp_dir("unsupported-start");
    let folder = root.join("Photos");
    std::fs::create_dir_all(&folder).unwrap();
    let refused = seed(&folder, 16);
    let before = refused_files(&folder);
    assert_eq!(before.len(), 4, "the catalog, its log, artifacts and index");

    let mut sheet = launch(prepared(&root, &refused, None), &refused).expect("the sheet opens");
    assert_eq!(
        refused_files(&folder),
        before,
        "the refusal changed nothing"
    );
    let new = folder.join("catalog (format 1).sqlite");
    assert_eq!(sheet.next.as_deref(), Some(new.as_path()));
    let model = sheet.model();
    assert!(
        model.formats.contains("catalog format 16"),
        "{}",
        model.formats
    );
    assert!(
        model.formats.contains("opens catalog format 1"),
        "{}",
        model.formats
    );
    assert!(model.start_enabled && model.quit_enabled);
    assert!(!new.exists(), "showing the sheet creates nothing");
    assert_eq!(stored(&root), None, "and stores nothing");

    // Start runs once: a second press while it runs does nothing.
    let Next::Stay(_) = sheet.update(Message::UnsupportedCatalog(Sheet::Start)) else {
        panic!("Start waits for the new catalog");
    };
    assert_eq!(sheet.phase, Phase::Starting);
    assert!(!sheet.model().start_enabled && !sheet.model().quit_enabled);
    let Next::Stay(_) = sheet.update(Message::UnsupportedCatalog(Sheet::Start)) else {
        panic!("a second Start does nothing");
    };

    // What the task Start queued runs here, as it runs off the update loop.
    luxforge_core::OwnerHandle::read_no_host_mounts(&new);
    let started = start_new(prepared(&root, &refused, None), &refused, 1).expect("started");
    assert_eq!(started.path, new);
    assert_eq!(started.unremembered, None);
    assert_eq!(stored(&root).unwrap()["catalog"], json!(new), "remembered");

    let mut shell = Shell::Unsupported(Box::new(sheet));
    let _ = shell.update(Message::UnsupportedCatalog(Sheet::Started(Ok(started))));
    let Shell::Editor(editor) = &mut shell else {
        panic!("the window becomes the editor");
    };
    assert_eq!(editor.preferences.catalog.path, new);
    assert_eq!(editor.preferences.catalog.forced, None);
    // A working workspace over the new catalog: a photograph develops into it and opens.
    let asset = crate::app::testing::import_and_adopt(
        &editor.owner,
        editor.client,
        &luxforge_testbase::paths::jpeg(),
    );
    let (state, _) = call(
        &editor.owner,
        editor.client,
        "asset.state",
        json!({"asset_id": asset}),
    )
    .expect("the new catalog answers");
    assert!(state.is_object(), "{state}");
    assert_eq!(
        call(&editor.owner, editor.client, "preferences.read", json!({}))
            .unwrap()
            .0["catalog"],
        json!(new)
    );
    editor.owner.stop();
    editor.owner_join.take().unwrap().join().unwrap();
    drop(shell);

    assert!(new.is_file());
    assert_eq!(
        refused_files(&folder),
        before,
        "the refused catalog, its log, artifacts and index keep every byte"
    );
    std::fs::remove_dir_all(root).unwrap();
}

/// Quit creates no catalog and writes no preference, and the refused catalog keeps every byte;
/// the window's close button is Quit.
#[test]
fn unsupported_catalog_quit_creates_and_writes_nothing() {
    let root = luxforge_testbase::paths::temp_dir("unsupported-quit");
    let refused = seed(&root, 2);
    let before = refused_files(&root);
    for message in [Message::UnsupportedCatalog(Sheet::Quit), Message::Close] {
        let mut sheet = launch(prepared(&root, &refused, None), &refused).unwrap();
        let Next::Stay(_) = sheet.update(message) else {
            panic!("Quit opens nothing");
        };
        assert_eq!(sheet.phase, Phase::Offered);
        assert_eq!(sheet.exit_problem(), None, "Quit is an ordinary exit");
    }
    assert!(!root.join("catalog (format 1).sqlite").exists());
    assert_eq!(stored(&root), None);
    assert_eq!(refused_files(&root), before);
    std::fs::remove_dir_all(root).unwrap();
}

/// A failed start says why and offers both choices again with the next free name; the refused
/// catalog keeps every byte.
#[test]
fn unsupported_catalog_a_failed_start_offers_both_choices_again() {
    let root = luxforge_testbase::paths::temp_dir("unsupported-failed");
    let refused = seed(&root, 16);
    let before = refused_files(&root);
    let mut sheet = launch(prepared(&root, &refused, None), &refused).unwrap();
    let _ = sheet.update(Message::UnsupportedCatalog(Sheet::Start));
    // The name was taken meanwhile by something that is not a catalog.
    let taken = root.join("catalog (format 1).sqlite");
    std::fs::write(&taken, b"not a catalog").unwrap();
    // The next free name is used instead: nothing that exists is opened.
    let second = root.join("catalog (format 1) 2.sqlite");
    luxforge_core::OwnerHandle::read_no_host_mounts(&second);
    let started = start_new(prepared(&root, &refused, None), &refused, 1).expect("started");
    assert_eq!(started.path, second);
    let boot = started.boot.lock().unwrap().take().unwrap();
    boot.owner.stop();
    boot.join.join().unwrap();
    assert!(second.is_file());
    assert_eq!(std::fs::read(&taken).unwrap(), b"not a catalog");
    let Next::Stay(_) = sheet.update(Message::UnsupportedCatalog(Sheet::Started(Err(
        "the folder is read-only".into(),
    )))) else {
        panic!("a failure keeps the sheet");
    };
    let model = sheet.model();
    assert_eq!(
        model.problem.as_deref(),
        Some("Could not start a new catalog: the folder is read-only")
    );
    assert!(model.start_enabled && model.quit_enabled);
    assert_eq!(
        sheet.next.as_deref(),
        Some(root.join("catalog (format 1) 3.sqlite").as_path())
    );
    assert_eq!(refused_files(&root), before);
    std::fs::remove_dir_all(root).unwrap();
}

/// `--catalog` keeps the command line's refusal, and so does another instance owning the
/// remembered catalog; an evidence run's own catalog is never offered a new one.
#[test]
fn unsupported_catalog_explicit_and_owned_catalogs_keep_the_command_line_refusal() {
    let root = luxforge_testbase::paths::temp_dir("unsupported-explicit");
    let refused = seed(&root, 16);
    let before = refused_files(&root);
    let explicit = launch(
        prepared(&root, &refused, Some(CatalogOverride::CommandLine)),
        &refused,
    )
    .err()
    .expect("refused on the command line");
    assert_eq!(
        explicit,
        format!(
            "cannot open catalog {}: incompatible: catalog format 16 is not supported; \
             expected 1; choose a new catalog path",
            refused.display()
        )
    );
    assert!(
        launch(
            prepared(&root, &refused, Some(CatalogOverride::Evidence)),
            &refused
        )
        .is_err()
    );
    assert_eq!(refused_files(&root), before);

    let owned = root.join("owned.sqlite");
    let holder = lifecycle::own(&prepared(&root, &owned, None), &owned).unwrap();
    let error = lifecycle::own(&prepared(&root, &owned, None), &owned)
        .err()
        .unwrap();
    assert_eq!(error.kind, ErrorKind::Conflict);
    assert_eq!(
        launch(prepared(&root, &owned, None), &owned).err().unwrap(),
        format!(
            "another Luxforge instance owns the catalog {}; close it or pass --catalog",
            owned.display()
        )
    );
    holder.0.stop();
    holder.1.join().unwrap();
    assert!(!root.join("catalog (format 1).sqlite").exists());
    std::fs::remove_dir_all(root).unwrap();
}

/// A catalog without the application mark is offered a new catalog too, and the sheet says it is
/// not marked rather than naming a format it may not have.
#[test]
fn unsupported_catalog_an_unmarked_catalog_is_named_as_unmarked() {
    let root = luxforge_testbase::paths::temp_dir("unsupported-unmarked");
    let catalog = root.join("catalog.sqlite");
    drop(luxforge_core::EditorService::open(&catalog).unwrap());
    rusqlite::Connection::open(&catalog)
        .unwrap()
        .pragma_update(None, "application_id", 0)
        .unwrap();
    let sheet = launch(prepared(&root, &catalog, None), &catalog).unwrap();
    assert!(!sheet.refusal.marked);
    assert!(
        sheet
            .model()
            .formats
            .starts_with("It is not marked as a Luxforge catalog of format 1"),
        "{}",
        sheet.model().formats
    );
    std::fs::remove_dir_all(root).unwrap();
}
