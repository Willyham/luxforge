//! The Settings sheet against a real owner: opening reads the flags, each change is written in
//! order and adopted, Escape and Cmd+, close it, and the window waits for the last write.
use super::{
    Boot, Editor, keymap,
    message::{Message, settings::SettingsMessage},
    tasks::{call, call_own},
};
use crate::state::{
    preferences::{GeneralControl, GeneralPreference, GeneralValue},
    settings::{FlagControl, SettingsTab},
};
use iced::{
    Event,
    event::Status,
    keyboard::{self, Key, Modifiers, key::Named},
};
use luxforge_core::flags::FlagList;
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};

/// An editor over a developer registry, so the proof flags are listed, with its preferences in a
/// directory of its own.
pub(super) fn launch() -> (Editor, PathBuf) {
    let root = luxforge_testbase::paths::temp_path("settings-sheet");
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
        config: crate::Config::default(),
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    (editor, root)
}

pub(super) fn finish(mut editor: Editor, root: PathBuf) {
    editor.owner.stop();
    if let Some(join) = editor.owner_join.take() {
        join.join().unwrap();
    }
    drop(editor);
    std::fs::remove_dir_all(root).unwrap();
}

/// Answer the sheet's read as its task would.
pub(super) fn answer_read(editor: &mut Editor) {
    let flags = call(&editor.owner, editor.client, "flags.list", json!({}))
        .map(|(value, _)| serde_json::from_value::<FlagList>(value).unwrap());
    let preferences = call(&editor.owner, editor.client, "preferences.read", json!({}))
        .map(|(value, _)| crate::state::preferences::parse(value).unwrap());
    let _ = editor.update(Message::Settings(SettingsMessage::Listed {
        flags,
        preferences,
    }));
}

/// Answer the preference writer's write in flight as its task would.
pub(super) fn answer_preference(editor: &mut Editor) {
    let params = editor
        .preferences
        .writing()
        .expect("a write in flight")
        .params();
    let answer = call_own(&editor.owner, editor.client, "preferences.set", params)
        .map(|(value, request)| (crate::state::preferences::parse(value).unwrap(), request));
    let _ = editor.update(Message::Preferences(
        super::message::preferences::PreferenceMessage::Saved(answer),
    ));
}

/// Answer the write in flight as its task would.
fn answer_write(editor: &mut Editor) {
    let (flag, value) = editor.settings.writing.clone().expect("a write in flight");
    let answer = call_own(
        &editor.owner,
        editor.client,
        "flags.set",
        json!({"flag": flag, "value": value}),
    )
    .map(|(value, request)| (serde_json::from_value::<FlagList>(value).unwrap(), request));
    let _ = editor.update(Message::Settings(SettingsMessage::Saved(answer)));
}

fn set(editor: &mut Editor, flag: &str, value: Option<Value>) {
    let _ = editor.update(Message::Settings(SettingsMessage::Set {
        flag: flag.into(),
        value,
    }));
}

#[test]
fn opening_reads_the_flags_and_each_change_is_written_in_order() {
    let (mut editor, root) = launch();
    assert!(editor.workspace.settings.open.is_none(), "closed at launch");
    let _ = editor.update(Message::Settings(SettingsMessage::Toggle));
    assert_eq!(
        editor.settings.open,
        Some(SettingsTab::General),
        "the first tab"
    );
    assert!(editor.settings.reading && editor.workspace.settings.loading);
    // The read already out answers for the tab chosen while it is.
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Experiments,
    )));
    assert_eq!(editor.settings.open, Some(SettingsTab::Experiments));
    assert!(editor.workspace.title.settings_open);
    answer_read(&mut editor);
    let ids: Vec<_> = editor
        .workspace
        .settings
        .rows
        .iter()
        .map(|row| row.id.as_str())
        .collect();
    assert_eq!(ids, ["developer", "proof.choice", "proof.number"]);

    set(&mut editor, "proof.choice", Some(json!("second")));
    set(&mut editor, "proof.number", Some(json!(75.0)));
    assert_eq!(
        editor.settings.writing,
        Some(("proof.choice".into(), Some(json!("second"))))
    );
    assert_eq!(editor.settings.waiting.len(), 1, "one write at a time");
    let rows = &editor.workspace.settings.rows;
    assert!(rows[1].saving && rows[2].saving, "both show at once");
    answer_write(&mut editor);
    assert_eq!(
        editor.settings.writing,
        Some(("proof.number".into(), Some(json!(75.0))))
    );
    answer_write(&mut editor);
    assert!(editor.settings.idle());
    let flags = editor.settings.flags.as_ref().unwrap();
    assert_eq!(flags.flag("proof.choice").unwrap().value, json!("second"));
    assert_eq!(flags.flag("proof.number").unwrap().value, json!(75.0));
    assert_eq!(
        editor.sync.own_requests.len(),
        2,
        "the sync skips the desktop's own writes"
    );

    // Reset removes the stored value; the snapshot records the row as drawn.
    set(&mut editor, "proof.choice", None);
    answer_write(&mut editor);
    let summary = editor.settings_summary();
    assert_eq!(summary["open"], "experiments");
    assert_eq!(summary["rows"][1]["control"], json!({"choice": "first"}));
    assert_eq!(summary["rows"][1]["can_reset"], false);
    assert_eq!(summary["writes_outstanding"], 0);
    assert!(
        editor.document.state.is_none(),
        "a flag needs no photograph or history"
    );
    finish(editor, root);
}

#[test]
fn a_number_field_sends_only_a_value_its_flag_takes() {
    let (mut editor, root) = launch();
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Experiments,
    )));
    answer_read(&mut editor);
    for (text, sent) in [("33", false), ("abc", false), ("40", true)] {
        let _ = editor.update(Message::Settings(SettingsMessage::NumberText {
            flag: "proof.number".into(),
            text: text.into(),
        }));
        let _ = editor.update(Message::Settings(SettingsMessage::NumberSubmit(
            "proof.number".into(),
        )));
        assert_eq!(editor.settings.writing.is_some(), sent, "{text}");
        if !sent {
            assert!(matches!(
                &editor.workspace.settings.rows[2].control,
                FlagControl::Number { invalid: true, .. }
            ));
            assert_eq!(
                editor.status.text,
                "Proof number takes a number from 0 to 100 in steps of 5"
            );
        }
    }
    answer_write(&mut editor);
    assert_eq!(
        editor
            .settings
            .flags
            .as_ref()
            .unwrap()
            .flag("proof.number")
            .unwrap()
            .value,
        json!(40.0)
    );
    finish(editor, root);
}

#[test]
fn a_refused_write_reports_why_and_reads_back_what_is_stored() {
    let (mut editor, root) = launch();
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Experiments,
    )));
    answer_read(&mut editor);
    set(&mut editor, "proof.choice", Some(json!("fourth")));
    answer_write(&mut editor);
    assert_eq!(
        editor.status.text,
        "Could not change proof.choice: validation: flag proof.choice: expected one of first, second, third"
    );
    assert!(editor.settings.reading, "the truth is read back");
    answer_read(&mut editor);
    assert!(matches!(
        &editor.workspace.settings.rows[1].control,
        FlagControl::Choice {
            selected: Some(0),
            ..
        }
    ));
    finish(editor, root);
}

#[test]
fn closing_the_window_waits_for_the_last_write() {
    let (mut editor, root) = launch();
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Experiments,
    )));
    answer_read(&mut editor);
    set(&mut editor, "developer", Some(json!(false)));
    let _ = editor.update(Message::Close);
    assert!(editor.settings.closing);
    assert!(editor.owner_join.is_some(), "the owner still runs");
    answer_write(&mut editor);
    assert!(
        editor.owner_join.is_none(),
        "the last write landed, so it closed"
    );
    let stored = std::fs::read_to_string(root.join("config").join("preferences.json")).unwrap();
    assert!(stored.contains("\"developer\": false"), "{stored}");
    finish(editor, root);
}

#[test]
fn the_sheet_is_modal_to_the_keyboard() {
    let press = |key: Key, modifiers: Modifiers| {
        Event::Keyboard(keyboard::Event::KeyPressed {
            key: key.clone(),
            modified_key: key,
            physical_key: keyboard::key::Physical::Unidentified(
                keyboard::key::NativeCode::Unidentified,
            ),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        })
    };
    let command = if cfg!(target_os = "macos") {
        Modifiers::LOGO
    } else {
        Modifiers::CTRL
    };
    let comma = || Key::Character(",".into());
    let closed = keymap::KeyContext::default();
    assert!(matches!(
        keymap::keymap(&press(comma(), command), Status::Ignored, &closed),
        Some(Message::Settings(SettingsMessage::Toggle))
    ));
    let open = keymap::KeyContext {
        settings_open: true,
        ..Default::default()
    };
    for (key, modifiers) in [
        (Key::Named(Named::Escape), Modifiers::empty()),
        (comma(), command),
    ] {
        assert!(matches!(
            keymap::keymap(&press(key, modifiers), Status::Ignored, &open),
            Some(Message::Settings(SettingsMessage::Close))
        ));
    }
    for (key, modifiers) in [
        (Key::Character("z".into()), command),
        (Key::Character("k".into()), command),
        (Key::Character("r".into()), Modifiers::empty()),
        (Key::Named(Named::Enter), Modifiers::empty()),
    ] {
        assert!(
            keymap::keymap(&press(key, modifiers), Status::Ignored, &open).is_none(),
            "the workspace behind the sheet takes no key"
        );
    }
}

#[test]
fn another_clients_flag_change_reaches_an_open_sheet_through_the_event_sync() {
    let (mut editor, root) = launch();
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Experiments,
    )));
    answer_read(&mut editor);
    let agent = editor.owner.register();
    call(
        &editor.owner,
        agent,
        "flags.set",
        json!({"flag": "proof.choice", "value": "second"}),
    )
    .unwrap();
    let polled = super::tasks::sync_now(
        &editor.owner,
        editor.client,
        None,
        editor.sync.sequence,
        &[],
        None,
    )
    .unwrap();
    assert!(polled.flags && polled.refresh.is_none(), "no asset is read");
    let _ = editor.update(Message::Sync(super::message::sync::SyncMessage::Synced(
        Ok(polled),
    )));
    assert!(
        editor.settings.reading,
        "the open sheet reads the flags again"
    );
    answer_read(&mut editor);
    assert!(matches!(
        &editor.workspace.settings.rows[1].control,
        FlagControl::Choice {
            selected: Some(1),
            ..
        }
    ));

    // A closed sheet reads nothing for it.
    let _ = editor.update(Message::Settings(SettingsMessage::Close));
    let closed = super::tasks::SyncResult {
        sequence: editor.sync.sequence,
        refresh: None,
        presets: None,
        capabilities: false,
        flags: true,
        preferences: false,
        themes: false,
        own: Vec::new(),
    };
    let _ = editor.update(Message::Sync(super::message::sync::SyncMessage::Synced(
        Ok(closed),
    )));
    assert!(!editor.settings.reading);
    finish(editor, root);
}

/// General's Auto collapse history switch starts on, writes the newest value asked for through
/// `preferences.set`, reaches the catalog writer at once — an edit of the control the last one
/// set then keeps both entries — and is read back on the next launch.
#[test]
fn auto_collapse_history_is_on_by_default_and_turning_it_off_keeps_every_entry() {
    let (mut editor, root) = launch();
    let _ = editor.update(Message::Settings(SettingsMessage::Toggle));
    answer_read(&mut editor);
    let collapse = |editor: &Editor| editor.workspace.settings.general[0].clone();
    assert_eq!(
        collapse(&editor).preference,
        GeneralPreference::AutoCollapseHistory
    );
    assert_eq!(collapse(&editor).control, GeneralControl::Toggle(true));

    // Off, on and off again before the first answer: two writes, the first and the newest.
    for on in [false, true, false] {
        let _ = editor.update(Message::Settings(SettingsMessage::SetGeneral(
            GeneralPreference::AutoCollapseHistory,
            GeneralValue::Toggle(on),
        )));
    }
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"auto_collapse_history": false})
    );
    assert_eq!(
        editor.preferences.waiting().unwrap().params(),
        json!({"auto_collapse_history": false})
    );
    assert!(collapse(&editor).saving);
    answer_preference(&mut editor);
    answer_preference(&mut editor);
    assert!(editor.preferences.idle());
    assert_eq!(collapse(&editor).control, GeneralControl::Toggle(false));
    assert_eq!(
        editor.settings_summary()["general"]["rows"][0],
        json!({"id": "auto_collapse_history", "control": {"toggle": false}, "saving": false})
    );

    // The owner applies it to the next edit without a relaunch.
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-2.jpg");
    let (asset, job) = super::testing::develop_and_prepare(&editor.owner, editor.client, &path);
    super::tasks::wait_source_job(&editor.owner, editor.client, &job).unwrap();
    let asset = json!(asset);
    for (revision, contrast) in [(0, 15.0), (1, 20.0)] {
        let (answer, _) = call(
            &editor.owner,
            editor.client,
            "edit.set-basic",
            json!({"asset_id": asset, "contrast": contrast, "mutation": super::tasks::mutation(revision)}),
        )
        .unwrap();
        assert!(answer.get("collapsed_entry_id").is_none(), "{answer}");
    }
    let (page, _) = call(
        &editor.owner,
        editor.client,
        "history.list",
        json!({"asset_id": asset}),
    )
    .unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 3);

    editor.owner.stop();
    if let Some(join) = editor.owner_join.take() {
        join.join().unwrap();
    }
    drop(editor);
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
    let (read, _) = call(&owner, owner.register(), "preferences.read", json!({})).unwrap();
    assert_eq!(
        read["auto_collapse_history"],
        json!(false),
        "kept across launches"
    );
    owner.stop();
    join.join().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}
