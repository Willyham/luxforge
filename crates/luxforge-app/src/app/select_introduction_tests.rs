//! The Select introduction's lifecycle on the desktop: shown once, the first time Select is shown in
//! a launch a person uses while the preference is off; every way of closing it storing the
//! preference once; the ? showing it again without touching it; never shown by itself in a test,
//! hidden or evidence launch; and the startup notices waiting for it. Owner answers are handed to
//! the update function as their tasks would hand them.
use super::{
    Boot, Editor, keymap,
    message::{
        Message, select::SelectMessage, select_introduction::IntroductionMessage,
        usage::UsageMessage,
    },
    settings_tests::{answer_preference, finish},
    tasks::call,
};
use crate::state::{
    select::Shown,
    select_introduction::{Closing, First},
    usage::Prompt,
};
use iced::{
    Event,
    event::Status,
    keyboard::{self, Key, Modifiers, key::Named},
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

/// An editor of a launch that shows the introduction by itself, or not, with its own preferences
/// directory, whose tool discovery has answered. Its catalog is empty, so it starts in Select,
/// though no message has run yet.
fn launch(introduces: bool) -> (Editor, PathBuf) {
    let root = luxforge_testbase::paths::temp_path("select-introduction");
    let host = luxforge_core::HostConfig {
        preferences_dir: Some(root.join("config")),
        ..luxforge_core::HostConfig::unconfigured()
    };
    let (owner, join) = luxforge_core::OwnerHandle::start_with_host(
        &root.join("catalog.sqlite"),
        Arc::new(luxforge_core::ModuleRegistry::builtin()),
        host,
    )
    .unwrap();
    let (mut editor, _) = Editor::new(Boot {
        owner,
        join,
        live_server: None,
        config: crate::Config {
            introduces,
            ..crate::Config::default()
        },
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    editor.modules_ready = true;
    (editor, root)
}

/// The launch as one that started in Develop, as a catalog with photographs does.
fn in_develop(editor: &mut Editor) {
    editor.select.state.shown = Shown::Develop;
}

fn show_select(editor: &mut Editor) {
    let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Select)));
    assert!(editor.select_shown());
}

/// Develop shown from the update function, as Develop N's photographs landing, `D`'s strip read
/// and an open from elsewhere show it, and the screen derived again.
fn leave_select(editor: &mut Editor) {
    let _ = editor.show_develop_workspace();
    editor.rederive();
}

/// Select's sources as read: no folder is indexed and no card is mounted.
fn nothing_indexed(editor: &mut Editor) {
    editor.select.state.cards =
        Some(luxforge_core::catalog_types::disk::Cards { cards: Vec::new() });
    editor.select.state.indexed = Some(luxforge_core::catalog_types::disk::IndexFolders {
        folders: Vec::new(),
    });
}

fn press(editor: &mut Editor, key: Key, modifiers: Modifiers) {
    let event = Event::Keyboard(keyboard::Event::KeyPressed {
        key: key.clone(),
        modified_key: key,
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers,
        text: None,
        repeat: false,
    });
    if let Some(message) = keymap::keymap(&event, Status::Ignored, &editor.key_context()) {
        let _ = editor.update(message);
    }
}

fn stored_seen(editor: &Editor) -> bool {
    let (answer, _) = call(&editor.owner, editor.client, "preferences.read", json!({})).unwrap();
    answer["select_introduction_seen"] == true
}

#[test]
fn select_introduction_is_shown_the_first_time_select_is_shown() {
    let (mut editor, root) = launch(true);
    assert!(editor.select_shown(), "an empty catalog starts in Select");
    in_develop(&mut editor);
    let _ = editor.update(Message::Introduction(IntroductionMessage::Close(
        Closing::Start,
    )));
    assert_eq!(
        editor.introduction.first,
        First::Waiting,
        "Develop shows nothing, and closing a sheet that is not open does nothing"
    );
    assert!(editor.workspace.select_introduction.is_none());
    show_select(&mut editor);
    assert_eq!(editor.introduction.first, First::Shown);
    let sheet = editor.workspace.select_introduction.clone().expect("drawn");
    assert_eq!(
        sheet.buttons(),
        ["Start browsing"],
        "Add a folder\u{2026} waits for the sources to be read"
    );
    nothing_indexed(&mut editor);
    editor.rederive();
    let sheet = editor.workspace.select_introduction.clone().expect("drawn");
    assert_eq!(sheet.title, "Select, then develop");
    assert_eq!(sheet.buttons(), ["Add a folder\u{2026}", "Start browsing"]);
    assert_eq!(editor.introduction_summary()["first"], "shown");
    assert!(editor.key_context().introduction);
    // Nothing is stored until it is closed, so quitting now shows it at the next launch.
    assert!(editor.preferences.idle());
    assert!(!stored_seen(&editor));
    finish(editor, root);
}

#[test]
fn select_introduction_every_way_of_closing_stores_the_preference_once() {
    for how in [
        Closing::Start,
        Closing::Return,
        Closing::Escape,
        Closing::Close,
        Closing::AddFolder,
    ] {
        let (mut editor, root) = launch(true);
        show_select(&mut editor);
        match how {
            Closing::Return => press(&mut editor, Key::Named(Named::Enter), Modifiers::empty()),
            Closing::Escape => press(&mut editor, Key::Named(Named::Escape), Modifiers::empty()),
            _ => {
                let _ = editor.update(Message::Introduction(IntroductionMessage::Close(how)));
            }
        }
        assert_eq!(editor.introduction.first, First::Closed(how), "{how:?}");
        assert!(editor.workspace.select_introduction.is_none());
        assert_eq!(
            editor.preferences.writing().map(|change| change.params()),
            Some(json!({"select_introduction_seen": true})),
            "{how:?}"
        );
        // Add a folder… goes on to the folder dialog, as the title bar's does.
        assert_eq!(editor.view_state.picker_open, how == Closing::AddFolder);
        answer_preference(&mut editor);
        assert!(stored_seen(&editor), "{how:?}");
        // Nothing brings the first showing back in this launch.
        let _ = editor.update(Message::Select(SelectMessage::Switch(Shown::Develop)));
        show_select(&mut editor);
        assert!(editor.workspace.select_introduction.is_none());
        assert!(editor.preferences.idle());
        finish(editor, root);
    }
}

#[test]
fn select_introduction_is_not_shown_once_seen_or_in_a_launch_that_never_shows_it() {
    let (mut editor, root) = launch(true);
    call(
        &editor.owner,
        editor.client,
        "preferences.set",
        json!({"select_introduction_seen": true}),
    )
    .unwrap();
    let preferences = call(&editor.owner, editor.client, "preferences.read", json!({}))
        .and_then(|(value, _)| crate::state::preferences::parse(value));
    editor.preferences.read(preferences);
    show_select(&mut editor);
    assert_eq!(editor.introduction.first, First::NotShown("seen"));
    assert!(editor.workspace.select_introduction.is_none());
    finish(editor, root);

    // A test, hidden, background or evidence launch never shows it by itself.
    let (mut editor, root) = launch(false);
    show_select(&mut editor);
    assert_eq!(editor.introduction.first, First::NotShown("launch"));
    assert!(editor.workspace.select_introduction.is_none());
    finish(editor, root);
}

#[test]
fn select_introduction_the_question_mark_shows_it_again_without_touching_the_preference() {
    let (mut editor, root) = launch(false);
    in_develop(&mut editor);
    let _ = editor.update(Message::Introduction(IntroductionMessage::Reopen));
    assert!(
        editor.workspace.select_introduction.is_none(),
        "the ? is Select's"
    );
    show_select(&mut editor);
    let _ = editor.update(Message::Introduction(IntroductionMessage::Reopen));
    assert!(editor.introduction.reopened);
    assert!(editor.workspace.select_introduction.is_some());
    press(&mut editor, Key::Named(Named::Escape), Modifiers::empty());
    assert!(editor.workspace.select_introduction.is_none());
    assert!(editor.preferences.idle(), "showing it again stores nothing");
    assert_eq!(editor.introduction.first, First::NotShown("launch"));
    finish(editor, root);
}

#[test]
fn select_introduction_holds_selects_keys_while_open() {
    let (mut editor, root) = launch(true);
    show_select(&mut editor);
    // `D` would switch to Develop and `Tab` hide the panels; neither reaches Select.
    let panels = editor.select.state.sources_panel;
    press(&mut editor, Key::Character("d".into()), Modifiers::empty());
    press(&mut editor, Key::Named(Named::Tab), Modifiers::empty());
    assert!(editor.select_shown());
    assert_eq!(editor.select.state.sources_panel, panels);
    // Develop N's chord is not the sheet's Return.
    press(&mut editor, Key::Named(Named::Enter), Modifiers::COMMAND);
    assert_eq!(editor.introduction.first, First::Shown);
    assert!(editor.develop.state.confirm.is_none());
    finish(editor, root);
}

/// Develop N's photographs landing, `D`'s strip read or an open from elsewhere can switch to
/// Develop while the sheet is up. Leaving Select takes it down, so it holds no key in Develop;
/// the person never closed it, so nothing is stored and the first showing comes again with Select.
#[test]
fn select_introduction_leaving_select_takes_it_down_and_it_comes_again() {
    let (mut editor, root) = launch(true);
    show_select(&mut editor);
    assert_eq!(editor.introduction.first, First::Shown);
    // As those paths do once their read lands: Develop is shown from the update function.
    leave_select(&mut editor);
    assert!(!editor.select_shown());
    assert_eq!(editor.introduction.first, First::Waiting);
    assert!(editor.workspace.select_introduction.is_none());
    assert!(!editor.key_context().introduction);
    assert!(editor.preferences.idle(), "nothing is stored");
    assert!(!stored_seen(&editor));
    // Develop's keys reach Develop: `G` shows Select, where the first showing comes again.
    press(&mut editor, Key::Character("g".into()), Modifiers::empty());
    assert!(editor.select_shown());
    assert_eq!(editor.introduction.first, First::Shown);
    assert!(editor.key_context().introduction);
    finish(editor, root);

    // One the ? brought back is simply gone.
    let (mut editor, root) = launch(false);
    show_select(&mut editor);
    let _ = editor.update(Message::Introduction(IntroductionMessage::Reopen));
    leave_select(&mut editor);
    assert!(!editor.introduction.open());
    show_select(&mut editor);
    assert!(editor.workspace.select_introduction.is_none());
    assert_eq!(editor.introduction.first, First::NotShown("launch"));
    finish(editor, root);
}

#[test]
fn select_introduction_the_startup_notices_wait_for_it() {
    let (mut editor, root) = launch(true);
    editor.usage.eligible = true;
    show_select(&mut editor);
    assert_eq!(editor.introduction.first, First::Shown);
    assert_eq!(
        editor.usage.prompt,
        Prompt::Waiting,
        "nothing is claimed while the sheet is up"
    );
    assert!(editor.move_question_delayed());
    let _ = editor.update(Message::Introduction(IntroductionMessage::Close(
        Closing::Start,
    )));
    assert_eq!(
        editor.usage.prompt,
        Prompt::Claiming,
        "claimed once it closes"
    );
    assert!(!editor.introduction_holds_notices());
    // A claimed notice waits for a sheet the ? shows again.
    let _ = editor.update(Message::Introduction(IntroductionMessage::Reopen));
    let read = crate::state::usage::UsageRead {
        prompt_shown: Some(true),
        enabled: Some(false),
        effective: false,
        disclosure_version: 1,
        destination: None,
        reason: Some("choice".into()),
        diagnostics: None,
    };
    let _ = editor.update(Message::Usage(UsageMessage::Claimed(Ok((
        true,
        read,
        "ui-claim".into(),
    )))));
    assert_eq!(editor.usage.prompt, Prompt::Pending);
    press(&mut editor, Key::Named(Named::Enter), Modifiers::empty());
    assert_eq!(editor.usage.prompt, Prompt::Shown);
    finish(editor, root);
}

#[test]
fn select_introduction_develops_notices_do_not_wait_for_a_sheet_select_has_not_shown() {
    let (mut editor, root) = launch(true);
    in_develop(&mut editor);
    editor.usage.eligible = true;
    let _ = editor.update(Message::Introduction(IntroductionMessage::Close(
        Closing::Start,
    )));
    assert!(!editor.select_shown());
    assert_eq!(editor.usage.prompt, Prompt::Claiming);
    finish(editor, root);
}
