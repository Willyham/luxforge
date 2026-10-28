//! Settings › Modules' external modules against a real owner with a module store: Install
//! module… reviews the picked package with `module.package.inspect` and installs it with
//! `module.install` from the sheet, the row then waits for Relaunch, Uninstall… confirms and
//! sends `module.uninstall`, a package the reader refuses says why with no Install, and the
//! sheet's keys put it away. The answers are run here as each task would run them.
use super::{
    Boot, Editor, keymap,
    message::{
        Message,
        settings::{ModulesMessage, SettingsMessage},
    },
    tasks::{call, call_detailed, call_own_detailed},
};
use crate::state::{
    external_modules::{Inspected, LoadState, RELAUNCH_TO_APPLY, ReviewState},
    module_settings::ModulesRead,
    settings::SettingsTab,
};
use iced::{
    Event,
    event::Status,
    keyboard::{self, Key, Modifiers, key::Named},
};
use luxforge_core::module_store::{ModuleHost, ModuleStore};
use serde_json::{Value, json};
use std::{
    io::{Cursor, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

const GLOW: &str = "org.example.glow";

/// A desktop over a fresh catalog whose host keeps a module store in its directory.
fn launch() -> (Editor, PathBuf) {
    let root = luxforge_testbase::paths::temp_dir("external-modules")
        .canonicalize()
        .unwrap();
    let host = luxforge_core::HostConfig {
        preferences_dir: Some(root.join("config")),
        modules: ModuleHost {
            store: Some(ModuleStore::new(root.join("data").join("modules"))),
            ..ModuleHost::default()
        },
        ..luxforge_core::HostConfig::unconfigured()
    };
    let (owner, join) = luxforge_core::OwnerHandle::start_with_host(
        &root.join("catalog.sqlite"),
        Arc::new(luxforge_core::ModuleRegistry::builtin()),
        host,
    )
    .unwrap();
    let (editor, _) = Editor::new(Boot {
        owner,
        join,
        live_server: None,
        config: crate::Config::default(),
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    (editor, root)
}

/// A `.lfmodule` package of the one-field Glow module at `version`, written into `dir`.
fn package(dir: &Path, version: &str) -> PathBuf {
    let manifest = format!(
        r#"
api = "0.1"
id = "{GLOW}"
name = "glow"
title = "Glow"
hint = "Glow the picture"
version = "{version}"
authors = ["Example"]
license = "MIT"
repository = "https://example.org/glow"
toolset = "adjustments"

[effect]
stage = "color"
format = 1

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
    );
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let stored =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, bytes) in [
        (
            luxforge_core::module_package::MANIFEST_FILE,
            manifest.as_bytes(),
        ),
        (
            luxforge_core::module_package::COMPONENT_FILE,
            b"\0asm\x0d\x00\x01\x00".as_slice(),
        ),
    ] {
        zip.start_file(name, stored).unwrap();
        zip.write_all(bytes).unwrap();
    }
    let path = dir.join(format!("glow-{version}.lfmodule"));
    std::fs::write(&path, zip.finish().unwrap().into_inner()).unwrap();
    path
}

fn modules(editor: &mut Editor, message: ModulesMessage) {
    let _ = editor.update(Message::Settings(SettingsMessage::Modules(message)));
}

/// Answer the tab's read as its task would.
fn answer_read(editor: &mut Editor) {
    assert!(editor.settings.modules.reading, "a read is in flight");
    let (owner, client) = (&editor.owner, editor.client);
    let read = call(owner, client, "module.list", json!({})).and_then(|(listed, _)| {
        let (layout, _) = call(owner, client, "module.layout.read", json!({}))?;
        let (packages, _) = call(owner, client, "module.package.list", json!({}))?;
        ModulesRead::parse(listed, layout)?.with_packages(packages)
    });
    modules(editor, ModulesMessage::Read(read.map(Box::new)));
}

/// Answer the review sheet's `module.package.inspect` as its task would.
fn answer_inspect(editor: &mut Editor) {
    let review = editor.settings.modules.review.as_ref().expect("a sheet");
    assert_eq!(review.state, ReviewState::Inspecting);
    let path = review.path.clone();
    let result = call_detailed(
        &editor.owner,
        editor.client,
        "module.package.inspect",
        json!({ "path": path }),
    )
    .map_err(|error| {
        let refusal = error
            .data
            .as_ref()
            .and_then(|data| data["refusal"].as_str())
            .map(str::to_owned);
        (error.message, refusal)
    })
    .and_then(|answer| {
        Inspected::parse(answer)
            .map(Box::new)
            .map_err(|e| (e, None))
    });
    modules(editor, ModulesMessage::Inspected { path, result });
}

/// Answer the lifecycle write in flight as its task would, returning what it sent.
fn answer_write(editor: &mut Editor) -> (&'static str, Value) {
    let write = editor
        .settings
        .modules
        .package_write
        .clone()
        .expect("a write in flight");
    let result = call_own_detailed(&editor.owner, editor.client, write.method(), write.params())
        .map_err(|error| error.message);
    modules(editor, ModulesMessage::PackageWritten(result));
    (write.method(), write.params())
}

fn summary(editor: &Editor) -> Value {
    editor.external_modules_summary()
}

fn press(key: Key) -> Event {
    Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat: false,
    })
}

fn finish(mut editor: Editor, root: PathBuf) {
    editor.owner.stop();
    if let Some(join) = editor.owner_join.take() {
        join.join().unwrap();
    }
    drop(editor);
    std::fs::remove_dir_all(root).unwrap();
}

/// Install module… opens Settings › Modules and the picker; the picked package is read by
/// `module.package.inspect` into the review sheet, whose Install sends `module.install` once; the
/// answer puts the sheet away and the row waits for Relaunch. Uninstall… asks first, then sends
/// `module.uninstall` once.
#[test]
fn external_module_install_reviews_installs_and_uninstall_confirms() {
    let (mut editor, root) = launch();
    let path = package(&root, "1.0.0");
    modules(&mut editor, ModulesMessage::Install);
    assert_eq!(editor.settings.open, Some(SettingsTab::Modules));
    assert!(editor.settings.modules.picking && editor.view_state.picker_open);
    answer_read(&mut editor);
    // While the picker is up, Install module… is disabled and opens nothing more.
    assert!(!editor.workspace.settings.modules.as_ref().unwrap().install);
    let _ = editor.external_modules_update(ModulesMessage::Install);
    assert!(editor.settings.modules.review.is_none());

    modules(
        &mut editor,
        ModulesMessage::InstallPicked(Some(path.clone())),
    );
    assert!(!editor.settings.modules.picking && !editor.view_state.picker_open);
    let drawn = editor.workspace.settings.modules.as_ref().unwrap();
    assert!(drawn.review.as_ref().unwrap().busy.is_some(), "reading");
    answer_inspect(&mut editor);
    let drawn = editor.workspace.settings.modules.as_ref().unwrap();
    let review = drawn.review.as_ref().expect("the review sheet");
    assert_eq!(review.title, "Install Glow 1.0.0?");
    assert!(
        review
            .facts
            .iter()
            .any(|(label, value)| label == "Network" && value == "None")
    );
    assert_eq!(review.confirm.as_ref().unwrap().label, "Install");
    assert_eq!(summary(&editor)["review"]["module"], GLOW);

    // Return presses the sheet's non-destructive Install.
    let context = editor.key_context();
    let message = keymap::keymap(&press(Key::Named(Named::Enter)), Status::Ignored, &context);
    assert!(matches!(
        message,
        Some(Message::Settings(SettingsMessage::Modules(
            ModulesMessage::Return
        )))
    ));
    let _ = editor.update(message.unwrap());
    // One write at a time: a second Install sends nothing more.
    modules(&mut editor, ModulesMessage::ReviewInstall);
    let (method, params) = answer_write(&mut editor);
    assert_eq!(method, "module.install");
    assert_eq!(params, json!({ "path": path }));
    assert!(
        editor.settings.modules.review.is_none(),
        "the sheet is put away"
    );
    assert!(
        editor.status.text.starts_with("Installed Glow 1.0.0"),
        "{}",
        editor.status.text
    );
    assert_eq!(summary(&editor)["answered"]["method"], "module.install");

    answer_read(&mut editor);
    let drawn = editor.workspace.settings.modules.as_ref().unwrap();
    assert_eq!(
        drawn.pending.as_deref(),
        Some("1 change applies when Luxforge relaunches.")
    );
    let row = drawn
        .groups
        .iter()
        .flat_map(|group| &group.rows)
        .find(|row| row.key.module == GLOW)
        .expect("the installed module's row before it is registered");
    let badges: Vec<&str> = row.badges.iter().map(|badge| badge.label).collect();
    assert_eq!(badges, ["Community", RELAUNCH_TO_APPLY]);
    let packages = &editor.settings.modules.read.as_ref().unwrap().packages;
    assert_eq!(
        packages.row(GLOW).unwrap().state,
        LoadState::PendingRelaunch
    );
    assert_eq!(summary(&editor)["pending"], drawn.pending.clone().unwrap());

    // Uninstall… asks, saying what the photos keep, and sends nothing until confirmed.
    modules(&mut editor, ModulesMessage::Uninstall(GLOW.into()));
    let drawn = editor.workspace.settings.modules.as_ref().unwrap();
    let ask = drawn.uninstall.as_ref().expect("the confirmation");
    assert_eq!(ask.title, "Uninstall Glow?");
    assert!(ask.body.contains("keep their edits"), "{}", ask.body);
    assert!(editor.settings.modules.package_write.is_none());
    // Return never presses a destructive button.
    modules(&mut editor, ModulesMessage::Return);
    assert!(editor.settings.modules.package_write.is_none());
    modules(&mut editor, ModulesMessage::UninstallConfirm);
    let (method, params) = answer_write(&mut editor);
    assert_eq!(method, "module.uninstall");
    assert_eq!(params, json!({ "module": GLOW }));
    assert!(editor.settings.modules.uninstall.is_none());
    answer_read(&mut editor);
    let listed = call(
        &editor.owner,
        editor.client,
        "module.package.list",
        json!({}),
    )
    .unwrap()
    .0;
    assert!(
        listed["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["installed"].is_null()),
        "{listed}"
    );
    finish(editor, root);
}

/// A package the reader refuses shows the owner's reason with no Install; Install sends nothing
/// and Escape puts the sheet away.
#[test]
fn external_module_a_refused_package_says_why_with_no_install() {
    let (mut editor, root) = launch();
    let path = root.join("broken.lfmodule");
    std::fs::write(&path, b"not a zip archive").unwrap();
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Modules,
    )));
    answer_read(&mut editor);
    modules(
        &mut editor,
        ModulesMessage::InstallPicked(Some(path.clone())),
    );
    answer_inspect(&mut editor);
    let drawn = editor.workspace.settings.modules.as_ref().unwrap();
    let review = drawn.review.as_ref().expect("the sheet");
    assert_eq!(review.title, "This package can't be installed");
    assert!(review.confirm.is_none() && review.dismiss == "Close");
    let reason = review.warning.clone().expect("the owner's reason");
    assert!(!reason.is_empty());
    assert_eq!(summary(&editor)["review"]["state"], "refused");
    modules(&mut editor, ModulesMessage::ReviewInstall);
    modules(&mut editor, ModulesMessage::Return);
    assert!(
        editor.settings.modules.package_write.is_none(),
        "nothing is sent"
    );

    let context = editor.key_context();
    let message = keymap::keymap(&press(Key::Named(Named::Escape)), Status::Ignored, &context);
    assert!(matches!(
        message,
        Some(Message::Settings(SettingsMessage::Modules(
            ModulesMessage::Dismiss
        )))
    ));
    let _ = editor.update(message.unwrap());
    assert!(editor.settings.modules.review.is_none());
    assert_eq!(
        editor.settings.open,
        Some(SettingsTab::Modules),
        "the sheet stays"
    );
    finish(editor, root);
}
