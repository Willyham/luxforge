//! The launch preferences on the desktop, against a real owner: the Catalog row shows the catalog
//! this launch opened with its notes and stores a chosen folder or the default without touching any
//! catalog, a missing catalog folder says so, and the close stores the window's frame in the
//! system's points, never in fullscreen or from a launch that does not remember it.
use super::{
    Boot, Editor,
    evidence::Settle,
    message::{Message, settings::SettingsMessage, view::ViewMessage},
    settings_tests::{answer_preference, finish},
};
use crate::{
    Config,
    state::{
        preferences::{
            GeneralControl, GeneralPreference, GeneralRow, GeneralValue, LaunchCatalog,
            PreferenceChange, general_rows,
        },
        settings::SettingsTab,
    },
    window_frame::WindowReport,
};
use luxforge_core::preferences::WindowFrame;
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

/// A desktop whose preferences live under `root/config` and whose owner holds `root/catalog.sqlite`,
/// started from `config`.
fn launch(config: Config) -> (Editor, PathBuf) {
    let root = luxforge_testbase::paths::temp_path("launch-preferences");
    let host = luxforge_core::HostConfig {
        preferences_dir: Some(root.join("config")),
        ..luxforge_core::HostConfig::unconfigured()
    };
    let (owner, join) = luxforge_core::OwnerHandle::start_with_host(
        &root.join("catalog.sqlite"),
        Arc::new(luxforge_core::ModuleRegistry::developer()),
        host,
    )
    .unwrap();
    let (editor, _) = Editor::new(Boot {
        owner,
        join,
        live_server: None,
        config,
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    (editor, root)
}

/// A launch that opened `path`, the default catalog being `default`.
fn opened(path: PathBuf, default: PathBuf) -> Config {
    Config {
        launch_catalog: Some(LaunchCatalog {
            path,
            default: Some(default),
            forced: None,
            missing: None,
        }),
        ..Config::default()
    }
}

fn catalog_row(editor: &Editor) -> GeneralRow {
    general_rows(&editor.preferences)
        .into_iter()
        .find(|row| row.preference == GeneralPreference::Catalog)
        .expect("the General tab shows the catalog")
}

fn stored(root: &std::path::Path) -> serde_json::Value {
    serde_json::from_str(
        &std::fs::read_to_string(root.join("config").join("preferences.json")).unwrap(),
    )
    .unwrap()
}

/// Choose Folder… opens one native dialog, whose folder stores `<folder>/catalog.sqlite` and a
/// relaunch note; Use Default stores null. Nothing creates, moves or deletes a catalog.
#[test]
fn the_catalog_row_stores_a_chosen_folder_or_the_default_and_moves_no_catalog() {
    let default = std::env::temp_dir()
        .join("launch-default")
        .join("catalog.sqlite");
    let (mut editor, root) = launch(opened(default.clone(), default.clone()));
    let row = catalog_row(&editor);
    assert_eq!(
        row.control,
        GeneralControl::Catalog {
            path: default.clone(),
            stored: None,
            notes: Vec::new(),
        }
    );

    // Choose Folder… opens the dialog once; a cancelled dialog changes nothing.
    let choose = |editor: &mut Editor| {
        let _ = editor.update(Message::Settings(SettingsMessage::SetGeneral(
            GeneralPreference::Catalog,
            GeneralValue::ChooseFolder,
        )));
    };
    choose(&mut editor);
    assert!(editor.view_state.picker_open);
    let _ = editor.update(Message::Settings(SettingsMessage::CatalogFolder(None)));
    assert!(!editor.view_state.picker_open && editor.preferences.idle());

    let folder = root.join("Photos");
    let chosen = folder.join("catalog.sqlite");
    choose(&mut editor);
    let _ = editor.update(Message::Settings(SettingsMessage::CatalogFolder(Some(
        folder.clone(),
    ))));
    assert!(!editor.view_state.picker_open);
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"catalog": chosen})
    );
    let row = catalog_row(&editor);
    assert!(row.saving);
    assert_eq!(
        row.control,
        GeneralControl::Catalog {
            path: default.clone(),
            stored: Some(chosen.clone()),
            notes: vec![format!("Relaunch to use {}", chosen.display())],
        }
    );
    answer_preference(&mut editor);
    assert_eq!(stored(&root)["catalog"], json!(chosen));
    assert!(
        !folder.exists(),
        "choosing a folder creates nothing; the next launch opens or creates the catalog"
    );
    assert!(
        root.join("catalog.sqlite").exists(),
        "the open catalog stays"
    );

    // The sheet's frame records the row as drawn.
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::General,
    )));
    let summary = editor.settings_summary();
    let rows = summary["general"]["rows"].as_array().unwrap();
    let recorded = rows.iter().find(|row| row["id"] == "catalog").unwrap();
    assert_eq!(
        recorded["control"],
        json!({"catalog": default, "stored": chosen,
               "notes": [format!("Relaunch to use {}", chosen.display())]})
    );

    // Use Default removes the stored location, so the default opens next.
    let _ = editor.update(Message::Settings(SettingsMessage::SetGeneral(
        GeneralPreference::Catalog,
        GeneralValue::UseDefault,
    )));
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"catalog": null})
    );
    answer_preference(&mut editor);
    assert!(stored(&root).get("catalog").is_none());
    let GeneralControl::Catalog { stored, notes, .. } = catalog_row(&editor).control else {
        panic!("a catalog control")
    };
    assert!(stored.is_none() && notes.is_empty());
    finish(editor, root);
}

/// A stored catalog whose folder was missing at launch says so in the status bar and on the row.
#[test]
fn a_missing_catalog_folder_says_so_in_the_status_bar_and_on_the_catalog_row() {
    let default = std::env::temp_dir()
        .join("launch-default")
        .join("catalog.sqlite");
    let unplugged = std::env::temp_dir()
        .join("launch-unplugged")
        .join("catalog.sqlite");
    let mut config = opened(default.clone(), default);
    config.launch_catalog.as_mut().unwrap().missing = Some(unplugged.clone());
    let (editor, root) = launch(config);
    let reason = format!(
        "Catalog folder not found: {}; using the default catalog",
        unplugged.parent().unwrap().display()
    );
    assert_eq!(editor.status.text, reason);
    let GeneralControl::Catalog { notes, .. } = catalog_row(&editor).control else {
        panic!("a catalog control")
    };
    assert_eq!(notes, [reason]);
    finish(editor, root);
}

/// The `preference` step chooses a catalog folder as its dialog would answer, for the catalog
/// file the preference stores, and Use Default for null; any other value fails the step.
#[test]
fn a_preference_step_chooses_a_catalog_folder_as_its_dialog_would() {
    let default = std::env::temp_dir()
        .join("launch-default")
        .join("catalog.sqlite");
    let (mut editor, root) = launch(opened(default.clone(), default));
    let chosen = root.join("Chosen").join("catalog.sqlite");
    let script = json!([
        {"preference": {"catalog": chosen}},
        {"preference": {"catalog": "relative/catalog.sqlite"}},
        {"preference": {"catalog": null}},
    ]);
    crate::app::testing::attach_script(&mut editor, &script.to_string());
    let _ = editor.next_step();
    assert_eq!(
        editor.evidence.as_ref().unwrap().awaiting,
        Some(Settle::Preferences)
    );
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"catalog": chosen})
    );
    assert!(
        !editor.view_state.picker_open,
        "no dialog in an evidence run"
    );
    answer_preference(&mut editor);
    assert!(editor.evidence.as_ref().unwrap().capture_pending);

    editor.evidence.as_mut().unwrap().capture_pending = false;
    let _ = editor.next_step();
    assert_eq!(
        editor.status.text,
        "catalog's control does not offer \"relative/catalog.sqlite\""
    );
    assert!(editor.preferences.idle());

    editor.evidence.as_mut().unwrap().capture_pending = false;
    let _ = editor.next_step();
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"catalog": null})
    );
    answer_preference(&mut editor);
    finish(editor, root);
}

fn report(width: f32, height: f32, fullscreen: bool) -> WindowReport {
    WindowReport {
        size: iced::Size::new(width, height),
        position: Some(iced::Point::new(-1200.5, 40.0)),
        display: Some(iced::Size::new(1512.0, 982.0)),
        fullscreen,
    }
}

fn remembering() -> Config {
    Config {
        remember_window: true,
        ..Config::default()
    }
}

/// The close asks Iced for the frame once, stores it in the system's points (Iced's logical size
/// times the interface size) through the writer and closes once the write has landed.
#[test]
fn the_window_frame_is_stored_at_close_in_the_systems_points() {
    let (mut editor, root) = launch(remembering());
    let _ = editor.store_preferences(PreferenceChange {
        interface_size: Some(125),
        ..PreferenceChange::default()
    });
    answer_preference(&mut editor);

    let _ = editor.update(Message::Close);
    assert!(editor.view_state.memory.asked);
    assert!(editor.owner_join.is_some(), "waits for Iced's answer");
    let _ = editor.update(Message::Close);
    assert!(
        editor.owner_join.is_some(),
        "a second close request waits too"
    );
    let _ = editor.update(Message::View(ViewMessage::ClosingFrame(Some(report(
        1000.0, 600.0, false,
    )))));
    assert!(editor.preferences.closing, "waits for the write");
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"window": {"width": 1250.0, "height": 750.0, "x": -1200.5, "y": 40.0}})
    );
    answer_preference(&mut editor);
    assert!(
        editor.owner_join.is_none(),
        "the write landed, so it closed"
    );
    assert_eq!(
        serde_json::from_value::<WindowFrame>(stored(&root)["window"].clone()).unwrap(),
        WindowFrame {
            width: 1250.0,
            height: 750.0,
            x: -1200.5,
            y: 40.0
        }
    );
    finish(editor, root);
}

/// A close in fullscreen keeps the frame stored before, a launch that does not remember its
/// window (hidden or evidence) never asks, and a window Iced cannot report stores nothing.
#[test]
fn a_fullscreen_or_hidden_window_stores_no_frame_at_close() {
    for (fullscreen_mode, fullscreen_state) in [(true, false), (false, true)] {
        let (mut editor, root) = launch(remembering());
        editor.view_state.fullscreen = fullscreen_state;
        let _ = editor.update(Message::Close);
        let _ = editor.update(Message::View(ViewMessage::ClosingFrame(Some(report(
            1000.0,
            600.0,
            fullscreen_mode,
        )))));
        assert!(editor.owner_join.is_none(), "closed without a write");
        assert!(!root.join("config").join("preferences.json").exists());
        finish(editor, root);
    }

    let (mut editor, root) = launch(remembering());
    let _ = editor.update(Message::Close);
    let _ = editor.update(Message::View(ViewMessage::ClosingFrame(None)));
    assert!(editor.owner_join.is_none());
    finish(editor, root);

    let (mut editor, root) = launch(Config::default());
    let _ = editor.update(Message::Close);
    assert!(!editor.view_state.memory.asked, "never asks for the frame");
    assert!(editor.owner_join.is_none(), "closed at once");
    assert!(!root.join("config").join("preferences.json").exists());
    finish(editor, root);
}
