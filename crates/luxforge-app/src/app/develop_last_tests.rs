//! Picking up where Develop was left, against a real owner over a catalog another client filled:
//! the Develop folder viewed and the photograph open are stored as `develop_last` with the
//! catalog's identity; a launch over it lands in Develop with the folder in the filmstrip, on the
//! remembered photograph or, when the folder no longer holds it, on the folder's first; a gone
//! folder opens the photograph with its own folder; and with nothing left, or another catalog's
//! preference, the launch stays as it would be with nothing remembered.
use super::{
    Boot, Editor,
    develop_last::remembered_place_now,
    message::{Message, develop::DevelopMessage, select::SelectMessage, sync::SyncMessage},
    settings_tests::{answer_preference, finish},
    tasks::{Scope, call, developed_photograph, queue_import, refresh},
    testing::descriptors,
};
use crate::state::select::Shown;
use luxforge_core::{
    AssetId, ClientId, OwnerHandle,
    catalog_types::{CatalogFolderId, ViewSource},
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

/// A catalog another client filled: photographs `a` and `b` in the catalog folder Konstanz, and an
/// empty folder Lake.
struct Catalog {
    owner: OwnerHandle,
    agent: ClientId,
    join: std::thread::JoinHandle<()>,
    root: PathBuf,
    catalog_id: String,
    konstanz: CatalogFolderId,
    lake: CatalogFolderId,
    a: AssetId,
    b: AssetId,
}

fn ok(owner: &OwnerHandle, agent: ClientId, method: &str, params: Value) -> Value {
    call(owner, agent, method, params)
        .unwrap_or_else(|error| panic!("{method}: {error}"))
        .0
}

fn mutation(id: &str) -> Value {
    json!({"request_id": id, "actor": "agent"})
}

fn catalog() -> Catalog {
    let root = luxforge_testbase::paths::temp_path("develop-last");
    let host = luxforge_core::HostConfig {
        preferences_dir: Some(root.join("config")),
        ..luxforge_core::HostConfig::unconfigured()
    };
    let (owner, join) = OwnerHandle::start_with_host(
        &root.join("catalog.sqlite"),
        Arc::new(luxforge_core::ModuleRegistry::builtin()),
        host,
    )
    .unwrap();
    let agent = owner.register();
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0");
    let mut developed = ["orientation-1.jpg", "orientation-2.jpg"].map(|name| {
        let queued = queue_import(&owner, agent, &fixtures.join(name)).unwrap();
        developed_photograph(&owner, agent, queued.job_id())
            .unwrap()
            .0
    });
    let folder = |name: &str| -> CatalogFolderId {
        let created = ok(
            &owner,
            agent,
            "folder.create",
            json!({"name": name, "mutation": mutation(name)}),
        );
        serde_json::from_value(created["folder"]["id"].clone()).unwrap()
    };
    let (konstanz, lake) = (folder("Konstanz"), folder("Lake"));
    ok(
        &owner,
        agent,
        "asset.move",
        json!({
            "targets": {"kind": "assets", "asset_ids": &developed},
            "folder_id": konstanz,
            "mutation": mutation("into-konstanz"),
        }),
    );
    let catalog_id = ok(&owner, agent, "catalog.info", json!({}))["catalog_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let [a, b] = std::mem::take(&mut developed);
    Catalog {
        owner,
        agent,
        join,
        root,
        catalog_id,
        konstanz,
        lake,
        a,
        b,
    }
}

impl Catalog {
    /// Move `asset` into `folder`, as another client does.
    fn move_to(&self, asset: &AssetId, folder: &CatalogFolderId) {
        ok(
            &self.owner,
            self.agent,
            "asset.move",
            json!({
                "targets": {"kind": "assets", "asset_ids": [asset]},
                "folder_id": folder,
                "mutation": mutation(&format!("move-{asset}")),
            }),
        );
    }

    /// Store `develop_last` as another client sets it.
    fn remember(&self, folder: Option<&CatalogFolderId>, asset: Option<&AssetId>) {
        ok(
            &self.owner,
            self.agent,
            "preferences.set",
            json!({"develop_last": {
                "catalog_id": self.catalog_id, "folder_id": folder, "asset_id": asset,
            }}),
        );
    }
}

/// The desktop launched over `catalog` with no file to open, and the catalog's facts.
fn launch(catalog: Catalog) -> (Editor, Facts) {
    let Catalog {
        owner,
        join,
        root,
        catalog_id,
        konstanz,
        lake,
        a,
        b,
        ..
    } = catalog;
    let (editor, _) = Editor::new(Boot {
        owner,
        join,
        live_server: None,
        config: crate::Config::default(),
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    (
        editor,
        Facts {
            root,
            catalog_id,
            konstanz,
            lake,
            a,
            b,
        },
    )
}

struct Facts {
    root: PathBuf,
    catalog_id: String,
    konstanz: CatalogFolderId,
    lake: CatalogFolderId,
    a: AssetId,
    b: AssetId,
}

/// Run the launch's read as its task would.
fn read_remembered(editor: &mut Editor) {
    let found = remembered_place_now(
        &editor.owner,
        editor.client,
        editor.preferences.applied_develop_last(),
    );
    let _ = editor.update(Message::Develop(DevelopMessage::Remembered(found)));
}

/// The photograph Develop is opening, and the filmstrip's folder and photographs.
fn landed(editor: &Editor) -> (Option<AssetId>, Option<CatalogFolderId>, Vec<AssetId>) {
    let strip = editor.develop.state.strip.as_ref();
    (
        editor
            .develop
            .switch
            .as_ref()
            .map(|switch| switch.asset.clone()),
        strip.and_then(|strip| strip.folder.clone()),
        strip.map_or_else(Vec::new, |strip| {
            strip
                .photos
                .iter()
                .map(|photo| photo.asset_id.clone())
                .collect()
        }),
    )
}

fn stored(editor: &Editor) -> Value {
    let agent = editor.owner.register();
    call(&editor.owner, agent, "preferences.read", json!({}))
        .unwrap()
        .0["develop_last"]
        .clone()
}

/// The folder viewed in Select and the photograph open in Develop are stored with the catalog's
/// identity, each as it changes.
#[test]
fn develop_last_stores_the_folder_viewed_and_the_photograph_open() {
    let (mut editor, facts) = launch(catalog());
    let refreshed = refresh(
        &editor.owner,
        editor.client,
        facts.b.clone(),
        Scope::Open,
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::ModulesLoaded(Ok(descriptors()))));
    let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
        refreshed,
    )))));
    assert!(editor.preferences.idle(), "nothing before the read");
    read_remembered(&mut editor);
    assert_eq!(
        editor.preferences.writing().map(|change| change.params()),
        Some(json!({"develop_last": {
            "catalog_id": facts.catalog_id, "folder_id": null, "asset_id": facts.b,
        }}))
    );
    answer_preference(&mut editor);
    let _ = editor.update(Message::Select(SelectMessage::Source(
        ViewSource::CatalogFolder {
            folder_id: facts.konstanz.clone(),
            subfolders: true,
        },
    )));
    answer_preference(&mut editor);
    assert_eq!(
        stored(&editor),
        json!({"catalog_id": facts.catalog_id, "folder_id": facts.konstanz, "asset_id": facts.b})
    );
    finish(editor, facts.root);
}

/// The remembered folder and photograph: Select views the folder, the filmstrip holds it and
/// Develop opens on the photograph, writing nothing back.
#[test]
fn develop_last_lands_on_the_remembered_photograph_in_its_folder() {
    let catalog = catalog();
    catalog.remember(Some(&catalog.konstanz), Some(&catalog.b));
    let (mut editor, facts) = launch(catalog);
    read_remembered(&mut editor);
    let (opening, folder, photos) = landed(&editor);
    assert_eq!(opening.as_ref(), Some(&facts.b));
    assert_eq!(folder.as_ref(), Some(&facts.konstanz));
    assert_eq!(photos.len(), 2);
    assert_eq!(editor.select.state.shown, Shown::Develop);
    assert_eq!(editor.develop_folder(), Some(&facts.konstanz));
    assert_eq!(
        editor
            .select
            .state
            .query
            .as_ref()
            .map(|query| &query.source),
        Some(&ViewSource::CatalogFolder {
            folder_id: facts.konstanz.clone(),
            subfolders: true,
        })
    );
    assert!(editor.preferences.idle(), "landing writes nothing back");
    finish(editor, facts.root);
}

/// A remembered photograph the folder no longer holds: Develop opens on the folder's first.
#[test]
fn develop_last_opens_the_folders_first_when_the_photograph_left_it() {
    let catalog = catalog();
    catalog.remember(Some(&catalog.konstanz), Some(&catalog.b));
    catalog.move_to(&catalog.b, &catalog.lake);
    let (mut editor, facts) = launch(catalog);
    read_remembered(&mut editor);
    assert_eq!(
        landed(&editor),
        (
            Some(facts.a.clone()),
            Some(facts.konstanz.clone()),
            vec![facts.a.clone()]
        )
    );
    finish(editor, facts.root);
}

/// A remembered folder that is gone: the photograph opens with its own folder in the filmstrip,
/// as a double-click opens it, and Select keeps no view.
#[test]
fn develop_last_opens_the_photograph_in_its_own_folder_when_the_folder_is_gone() {
    let catalog = catalog();
    catalog.remember(Some(&CatalogFolderId::new()), Some(&catalog.b));
    catalog.move_to(&catalog.b, &catalog.lake);
    let (mut editor, facts) = launch(catalog);
    read_remembered(&mut editor);
    assert_eq!(
        landed(&editor),
        (
            Some(facts.b.clone()),
            Some(facts.lake.clone()),
            vec![facts.b.clone()]
        )
    );
    assert_eq!(editor.develop_folder(), Some(&facts.lake));
    assert!(editor.select.state.query.is_none());
    finish(editor, facts.root);
}

/// Nothing left of where Develop was, or another catalog's preference: the launch stays as it
/// would be with nothing remembered, saying nothing, and the preference is kept.
#[test]
fn develop_last_that_nothing_survives_leaves_the_launch_as_it_was() {
    for another_catalog in [false, true] {
        let catalog = catalog();
        if another_catalog {
            ok(
                &catalog.owner,
                catalog.agent,
                "preferences.set",
                json!({"develop_last": {
                    "catalog_id": "another-catalog", "folder_id": catalog.konstanz,
                    "asset_id": catalog.a,
                }}),
            );
        } else {
            catalog.remember(Some(&CatalogFolderId::new()), Some(&AssetId::new()));
        }
        let (mut editor, facts) = launch(catalog);
        let before = stored(&editor);
        let (status, shown) = (editor.status.text.clone(), editor.select.state.shown);
        read_remembered(&mut editor);
        assert_eq!(landed(&editor), (None, None, Vec::new()));
        assert_eq!(editor.develop_folder(), None);
        assert!(editor.select.state.query.is_none());
        assert_eq!(
            (editor.status.text.clone(), editor.select.state.shown),
            (status, shown)
        );
        assert!(editor.preferences.idle());
        assert_eq!(stored(&editor), before);
        finish(editor, facts.root);
    }
}
