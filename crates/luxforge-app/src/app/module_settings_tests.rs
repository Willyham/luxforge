//! Settings › Modules against a real owner: opening reads the modules and the layout, each gesture
//! sends exactly one layout write and adopts its answer, a refused write says why and reads back
//! what is stored, Undo and the resets write back, another client's change reaches the open tab
//! through the event sync, and no recipe changes.
use super::{
    Editor, keymap,
    message::{
        Message,
        palette::PaletteMessage,
        settings::{ModuleDragEdit, ModulesKey, ModulesMessage, SettingsMessage},
        sync::SyncMessage,
    },
    settings_tests::{finish, launch},
    tasks::{call, call_own},
};
use crate::state::{
    module_settings::{LayoutWrite, ModulesFocus, ModulesRead, Move, RowControl, RowKey},
    palette::PaletteAction,
    settings::SettingsTab,
};
use iced::{
    Event,
    event::Status,
    keyboard::{self, Key, Modifiers, key::Named},
};
use luxforge_core::Toolset;
use serde_json::{Value, json};

fn modules(editor: &mut Editor, message: ModulesMessage) {
    let _ = editor.update(Message::Settings(SettingsMessage::Modules(message)));
}

/// Answer the tab's read as its task would.
fn answer_read(editor: &mut Editor) {
    assert!(editor.settings.modules.reading, "a read is in flight");
    let read = call(&editor.owner, editor.client, "module.list", json!({}))
        .and_then(|(listed, _)| {
            let (layout, _) = call(
                &editor.owner,
                editor.client,
                "module.layout.read",
                json!({}),
            )?;
            ModulesRead::parse(listed, layout)
        })
        .map(Box::new);
    modules(editor, ModulesMessage::Read(read));
}

/// Answer the layout write in flight as its task would, returning what it sent.
fn answer_write(editor: &mut Editor) -> (String, Value) {
    let write = editor
        .settings
        .modules
        .writing
        .clone()
        .expect("a write in flight");
    let answer = call_own(&editor.owner, editor.client, write.method(), write.params()).and_then(
        |(answer, request)| {
            serde_json::from_value(answer)
                .map(|layout| (layout, request))
                .map_err(|error| error.to_string())
        },
    );
    modules(editor, ModulesMessage::Written(answer));
    (write.method().to_owned(), write.params())
}

fn layout(editor: &Editor) -> Value {
    call(
        &editor.owner,
        editor.client,
        "module.layout.read",
        json!({}),
    )
    .unwrap()
    .0
}

fn open(editor: &mut Editor) {
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Modules,
    )));
    answer_read(editor);
}

fn adjustments(editor: &Editor) -> Vec<String> {
    editor.workspace.settings.modules.as_ref().unwrap().groups[0]
        .rows
        .iter()
        .map(|row| row.key.module.clone())
        .collect()
}

fn row(toolset: Toolset, module: &str) -> RowKey {
    RowKey {
        toolset,
        module: module.into(),
    }
}

/// The switch writes `module.disable` once and adopts its answer; Undo and Cmd+Z write back
/// `module.enable`; a locked row's lock writes nothing and says why in the detail pane.
#[test]
fn settings_modules_switch_writes_once_and_undo_writes_it_back() {
    let (mut editor, root) = launch();
    open(&mut editor);
    let tab = editor.workspace.settings.modules.as_ref().unwrap();
    assert!(!tab.loading && tab.groups.len() >= 4);
    assert_eq!(editor.workspace.settings.tabs[2], SettingsTab::Modules);

    modules(
        &mut editor,
        ModulesMessage::Switch {
            module: "luxforge.vignette".into(),
            on: false,
        },
    );
    assert!(
        editor.settings.modules.waiting.is_empty(),
        "one write, in flight"
    );
    assert_eq!(
        answer_write(&mut editor),
        (
            "module.disable".into(),
            json!({"module": "luxforge.vignette"})
        )
    );
    assert!(editor.settings.modules.idle());
    assert_eq!(layout(&editor)["disabled"], json!(["luxforge.vignette"]));
    let tab = editor.workspace.settings.modules.as_ref().unwrap();
    assert_eq!(tab.toast.as_deref(), Some("Turned Vignette off"));
    assert_eq!(
        editor.status.text,
        "Turned Vignette off \u{b7} rendering unchanged"
    );
    let summary = editor.settings_summary();
    assert_eq!(summary["modules"]["toast"], json!("Turned Vignette off"));
    assert_eq!(
        summary["modules"]["layout"]["disabled"],
        json!(["luxforge.vignette"])
    );

    // Cmd+Z while the toast shows is Undo.
    let undo = keymap::keymap(
        &press(Key::Character("z".into()), command()),
        Status::Ignored,
        &editor.key_context(),
    );
    assert!(matches!(
        undo,
        Some(Message::Settings(SettingsMessage::Modules(
            ModulesMessage::Undo
        )))
    ));
    let _ = editor.update(undo.unwrap());
    assert_eq!(
        answer_write(&mut editor),
        (
            "module.enable".into(),
            json!({"module": "luxforge.vignette"})
        )
    );
    assert_eq!(layout(&editor)["disabled"], json!([]));
    assert_eq!(
        editor.workspace.settings.modules.as_ref().unwrap().toast,
        None
    );
    assert!(
        keymap::keymap(
            &press(Key::Character("z".into()), command()),
            Status::Ignored,
            &editor.key_context(),
        )
        .is_none(),
        "with no toast Cmd+Z does nothing"
    );

    // A locked module's lock writes nothing and says why.
    modules(
        &mut editor,
        ModulesMessage::Locked(row(Toolset::Adjustments, "luxforge.basic")),
    );
    assert!(editor.settings.modules.idle());
    let detail = editor
        .workspace
        .settings
        .modules
        .as_ref()
        .unwrap()
        .detail
        .clone()
        .unwrap();
    assert_eq!(detail.title, "Basic");
    assert!(
        detail
            .refusal
            .is_some_and(|reason| reason.starts_with("Basic is always on"))
    );
    finish(editor, root);
}

/// The keyboard selects, moves within a group and toggles; a drag writes once on release and
/// Escape lets it go with nothing written; Reset order and Reset all are one write each.
#[test]
fn settings_modules_keyboard_drag_and_resets_each_write_once() {
    let (mut editor, root) = launch();
    open(&mut editor);
    let context = editor.key_context();
    let key = |key: Named, modifiers: Modifiers| {
        keymap::keymap(
            &press(Key::Named(key), modifiers),
            Status::Ignored,
            &context,
        )
    };
    assert!(matches!(
        key(Named::ArrowDown, Modifiers::empty()),
        Some(Message::Settings(SettingsMessage::Modules(
            ModulesMessage::Key(ModulesKey::Step(1))
        )))
    ));
    assert!(matches!(
        key(Named::ArrowUp, Modifiers::ALT),
        Some(Message::Settings(SettingsMessage::Modules(
            ModulesMessage::Key(ModulesKey::Move(Move::Up))
        )))
    ));
    assert!(matches!(
        key(Named::ArrowDown, Modifiers::ALT | Modifiers::SHIFT),
        Some(Message::Settings(SettingsMessage::Modules(
            ModulesMessage::Key(ModulesKey::Move(Move::Bottom))
        )))
    ));
    assert!(matches!(
        key(Named::Space, Modifiers::empty()),
        Some(Message::Settings(SettingsMessage::Modules(
            ModulesMessage::Key(ModulesKey::Toggle)
        )))
    ));
    assert!(matches!(
        key(Named::Escape, Modifiers::empty()),
        Some(Message::Settings(SettingsMessage::Close))
    ));

    let before = adjustments(&editor);
    modules(
        &mut editor,
        ModulesMessage::Select(row(Toolset::Adjustments, "luxforge.curve")),
    );
    modules(&mut editor, ModulesMessage::Key(ModulesKey::Move(Move::Up)));
    let (method, params) = answer_write(&mut editor);
    assert_eq!(method, "module.order.set");
    let mut moved = before.clone();
    let at = moved.iter().position(|id| id == "luxforge.curve").unwrap();
    moved.swap(at - 1, at);
    assert_eq!(params, json!({"toolset": "adjustments", "order": moved}));
    assert_eq!(adjustments(&editor), moved);
    assert_eq!(layout(&editor)["toolsets"][0]["order"], json!(moved));

    // Space toggles the selected module.
    modules(&mut editor, ModulesMessage::Key(ModulesKey::Toggle));
    assert_eq!(answer_write(&mut editor).0, "module.disable");

    // A drag: Escape lets go with nothing written; a release over another row writes once.
    let curve = row(Toolset::Adjustments, "luxforge.curve");
    modules(
        &mut editor,
        ModulesMessage::Drag(ModuleDragEdit::Start(curve.clone())),
    );
    let dragging = editor.key_context();
    assert!(matches!(
        keymap::keymap(
            &press(Key::Named(Named::Escape), Modifiers::empty()),
            Status::Ignored,
            &dragging
        ),
        Some(Message::Settings(SettingsMessage::Modules(
            ModulesMessage::Drag(ModuleDragEdit::Cancel)
        )))
    ));
    modules(
        &mut editor,
        ModulesMessage::Drag(ModuleDragEdit::Over("luxforge.vignette".into())),
    );
    modules(&mut editor, ModulesMessage::Drag(ModuleDragEdit::Cancel));
    modules(&mut editor, ModulesMessage::Drag(ModuleDragEdit::End));
    assert!(
        editor.settings.modules.idle(),
        "a cancelled drag writes nothing"
    );
    let last = adjustments(&editor).last().cloned().unwrap();
    modules(
        &mut editor,
        ModulesMessage::Drag(ModuleDragEdit::Start(curve)),
    );
    modules(
        &mut editor,
        ModulesMessage::Drag(ModuleDragEdit::Over(last)),
    );
    modules(&mut editor, ModulesMessage::Drag(ModuleDragEdit::End));
    let (method, params) = answer_write(&mut editor);
    assert_eq!(method, "module.order.set");
    assert_eq!(
        params["order"].as_array().unwrap().last(),
        Some(&json!("luxforge.curve"))
    );
    assert!(editor.settings.modules.drag.is_none());

    // Reset order, then Reset all; Undo of Reset all writes back each order and switch.
    modules(
        &mut editor,
        ModulesMessage::ResetOrder(Toolset::Adjustments),
    );
    assert_eq!(
        answer_write(&mut editor),
        (
            "module.layout.reset".into(),
            json!({"toolset": "adjustments"})
        )
    );
    assert_eq!(adjustments(&editor), before);
    modules(&mut editor, ModulesMessage::ResetAll);
    assert_eq!(
        answer_write(&mut editor),
        ("module.layout.reset".into(), json!({}))
    );
    assert_eq!(layout(&editor)["disabled"], json!([]));
    modules(&mut editor, ModulesMessage::Undo);
    assert_eq!(
        answer_write(&mut editor),
        ("module.disable".into(), json!({"module": "luxforge.curve"}))
    );
    assert!(editor.settings.modules.idle());
    assert_eq!(layout(&editor)["disabled"], json!(["luxforge.curve"]));
    finish(editor, root);
}

/// A write the host refuses says why, drops the toast and reads back what is stored.
#[test]
fn settings_modules_a_refused_write_says_why_and_reads_back() {
    let (mut editor, root) = launch();
    open(&mut editor);
    editor
        .settings
        .modules
        .waiting
        .push_back(LayoutWrite::Disable("luxforge.basic".into()));
    modules(&mut editor, ModulesMessage::Key(ModulesKey::Move(Move::Up)));
    // Nothing selected moves nothing; the queued write goes out with the next gesture.
    modules(
        &mut editor,
        ModulesMessage::Switch {
            module: "luxforge.vignette".into(),
            on: false,
        },
    );
    let _ = answer_write(&mut editor);
    assert!(
        editor
            .status
            .text
            .starts_with("Could not change the module layout")
    );
    assert!(
        editor.settings.modules.reading,
        "it reads back what is stored"
    );
    assert_eq!(editor.settings.modules.toast, None);
    assert!(matches!(
        &editor.settings.modules.refusal,
        Some((Some(module), reason)) if module == "luxforge.basic" && reason.contains("locked")
    ));
    // The change made after it still lands.
    let _ = answer_write(&mut editor);
    answer_read(&mut editor);
    assert_eq!(layout(&editor)["disabled"], json!(["luxforge.vignette"]));
    let vignette = editor.workspace.settings.modules.as_ref().unwrap().groups[0]
        .rows
        .iter()
        .find(|row| row.key.module == "luxforge.vignette")
        .unwrap()
        .control
        .clone();
    assert_eq!(vignette, RowControl::Switch(false));
    finish(editor, root);
}

/// Another client's change reaches the open tab through the event sync; this desktop's own writes
/// are skipped; the tab changes no recipe and announces only layout events.
#[test]
fn settings_modules_follows_another_clients_change_and_changes_no_recipe() {
    let (mut editor, root) = launch();
    let start = editor.sync.sequence;
    open(&mut editor);
    modules(
        &mut editor,
        ModulesMessage::Switch {
            module: "luxforge.mixer".into(),
            on: false,
        },
    );
    let _ = answer_write(&mut editor);
    let own: Vec<String> = editor.sync.own_requests.iter().cloned().collect();
    let polled =
        super::tasks::sync_now(&editor.owner, editor.client, None, start, &own, None).unwrap();
    assert!(!polled.module_layout, "this desktop's own write is skipped");

    let agent = editor.owner.register();
    call(
        &editor.owner,
        agent,
        "module.disable",
        json!({"module": "luxforge.vignette"}),
    )
    .unwrap();
    let polled =
        super::tasks::sync_now(&editor.owner, editor.client, None, start, &own, None).unwrap();
    assert!(
        polled.module_layout && polled.refresh.is_none(),
        "no asset is read"
    );
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled))));
    answer_read(&mut editor);
    let off: Vec<String> = editor.workspace.settings.modules.as_ref().unwrap().groups[0]
        .rows
        .iter()
        .filter(|row| row.off)
        .map(|row| row.key.module.clone())
        .collect();
    assert_eq!(off, ["luxforge.mixer", "luxforge.vignette"]);

    // Every event the tab caused is a layout event naming no photo.
    let (events, _) = call(
        &editor.owner,
        editor.client,
        "events.since",
        json!({"after": start}),
    )
    .unwrap();
    let events = events["events"].as_array().unwrap();
    assert!(!events.is_empty());
    assert!(events.iter().all(|event| event["method"] == "module.layout"
        && event.get("asset_id").is_none_or(Value::is_null)));
    assert!(editor.document.state.is_none() && editor.crop().is_none());

    // A closed tab reads nothing for it.
    let _ = editor.update(Message::Settings(SettingsMessage::Close));
    let _ = editor.module_layout_changed_elsewhere(true);
    assert!(!editor.settings.modules.reading);
    finish(editor, root);
}

/// Customize opens Settings at Modules with the module it came from selected, or a group's first
/// row; the palette lists Settings · Modules.
#[test]
fn settings_modules_opens_at_the_module_customize_came_from() {
    let (mut editor, root) = launch();
    let _ = editor.update(Message::Settings(SettingsMessage::OpenModules(Some(
        ModulesFocus::Module("luxforge.vignette".into()),
    ))));
    assert_eq!(editor.settings.open, Some(SettingsTab::Modules));
    answer_read(&mut editor);
    assert_eq!(
        editor.settings.modules.selected,
        Some(row(Toolset::Adjustments, "luxforge.vignette"))
    );
    let _ = editor.update(Message::Settings(SettingsMessage::Close));
    let _ = editor.update(Message::Settings(SettingsMessage::OpenModules(Some(
        ModulesFocus::Toolset(Toolset::Tools),
    ))));
    assert_eq!(
        editor.settings.modules.selected,
        Some(row(Toolset::Tools, "luxforge.masks")),
        "what the tab read last selects at once"
    );
    let _ = editor.update(Message::Settings(SettingsMessage::Close));
    let _ = editor.update(Message::Palette(PaletteMessage::Open));
    let _ = editor.update(Message::Palette(PaletteMessage::Query("modules".into())));
    let entry = editor
        .workspace
        .palette
        .entries
        .iter()
        .find(|entry| entry.label == "Settings \u{b7} Modules")
        .expect("the palette lists Settings · Modules")
        .clone();
    assert_eq!(entry.action, PaletteAction::Settings(SettingsTab::Modules));
    let _ = editor.update(Message::Palette(PaletteMessage::Run));
    assert_eq!(editor.settings.open, Some(SettingsTab::Modules));
    assert!(editor.settings.modules.reading, "opening the tab reads it");
    finish(editor, root);
}

fn command() -> Modifiers {
    if cfg!(target_os = "macos") {
        Modifiers::LOGO
    } else {
        Modifiers::CTRL
    }
}

fn press(key: Key, modifiers: Modifiers) -> Event {
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
}
