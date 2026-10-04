//! The desktop's one preference writer against a real owner: every control's change goes through
//! it one call at a time with later changes merged, a refusal says why and puts the stored value
//! back, closing waits for the last write, the mask overlay colour is both this session's and a
//! stored preference, another client's change is read again and applied, and the `preference`
//! evidence step drives the General rows' own messages.
use super::{
    Editor,
    evidence::Settle,
    message::{
        Message, mask::MaskMessage, performance::PerformanceMessage,
        preferences::PreferenceMessage, settings::SettingsMessage, sync::SyncMessage,
    },
    settings_tests::{answer_preference, answer_read, finish, launch},
    tasks::{call, sync_now},
};
use crate::state::{
    preferences::{GeneralControl, GeneralPreference, GeneralValue, PreferenceChange, parse},
    settings::SettingsTab,
};
use luxforge_core::{AssetId, MaskOverlayColour};
use serde_json::json;

fn general(editor: &mut Editor, row: GeneralPreference, value: GeneralValue) {
    let _ = editor.update(Message::Settings(SettingsMessage::SetGeneral(row, value)));
}

/// What the host stores, read as another client would.
fn read(editor: &Editor) -> serde_json::Value {
    let agent = editor.owner.register();
    call(&editor.owner, agent, "preferences.read", json!({}))
        .unwrap()
        .0
}

/// Poll the event log as the event sync does and hand the answer to the desktop.
fn poll(editor: &mut Editor) -> super::tasks::SyncResult {
    let own: Vec<String> = editor.sync.own_requests.iter().cloned().collect();
    let polled = sync_now(
        &editor.owner,
        editor.client,
        (AssetId::new(), 0),
        editor.sync.sequence,
        &own,
        None,
    )
    .unwrap();
    let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled.clone()))));
    polled
}

/// Answer the writer's read of the preferences as its task would.
fn answer_preferences_read(editor: &mut Editor) {
    assert!(editor.preferences.reading.in_flight(), "a read in flight");
    let answer = call(&editor.owner, editor.client, "preferences.read", json!({}))
        .and_then(|(v, _)| parse(v));
    let _ = editor.update(Message::Preferences(PreferenceMessage::Read(answer)));
}

/// The Performance disclosure and the General rows all write through the one writer: one call in
/// flight, every later change merged into one waiting change with the newer value of a field
/// replacing the older, each shown at once, and the desktop's own announced writes skipped by the
/// event sync.
#[test]
fn every_preference_write_goes_through_one_writer_one_call_at_a_time() {
    let (mut editor, root) = launch();
    assert!(
        editor.preferences.stored().is_some(),
        "the whole answer is held from launch"
    );
    general(
        &mut editor,
        GeneralPreference::AutoCollapseHistory,
        GeneralValue::Toggle(false),
    );
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"auto_collapse_history": false})
    );
    let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
    general(
        &mut editor,
        GeneralPreference::AutoLensProfile,
        GeneralValue::Toggle(false),
    );
    general(
        &mut editor,
        GeneralPreference::AutoCollapseHistory,
        GeneralValue::Toggle(true),
    );
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"auto_collapse_history": false}),
        "one call in flight"
    );
    assert_eq!(
        editor.preferences.waiting().unwrap().params(),
        json!({"performance_expanded": false, "auto_collapse_history": true,
               "auto_lens_profile": false})
    );
    let applied = editor.preferences.applied().unwrap();
    assert!(
        applied.auto_collapse_history
            && !applied.auto_lens_profile
            && !applied.performance_expanded,
        "shown at once"
    );
    assert_eq!(editor.settings_summary()["writes_outstanding"], 2);

    answer_preference(&mut editor);
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"performance_expanded": false, "auto_collapse_history": true,
               "auto_lens_profile": false}),
        "the merged change goes next"
    );
    assert!(editor.preferences.waiting().is_none());
    answer_preference(&mut editor);
    assert!(editor.preferences.idle());
    let stored = read(&editor);
    assert_eq!(
        (
            &stored["performance_expanded"],
            &stored["auto_collapse_history"],
            &stored["auto_lens_profile"]
        ),
        (&json!(false), &json!(true), &json!(false))
    );
    assert_eq!(
        editor.sync.own_requests.len(),
        2,
        "the sync skips the desktop's own announced writes"
    );
    let polled = poll(&mut editor);
    assert!(
        !polled.preferences,
        "the desktop's own writes are not read again"
    );
    assert!(!editor.preferences.reading.in_flight());
    let summary = editor.preferences_summary();
    assert_eq!(summary["applied"]["auto_lens_profile"], false);
    assert_eq!(summary["writing"], json!(null));
    finish(editor, root);
}

/// A change the host refuses says why in the status bar, and the value stored shows again; the
/// desktop reads back what is stored.
#[test]
fn a_refused_preference_write_says_why_and_puts_the_stored_value_back() {
    let (mut editor, root) = launch();
    let _ = editor.store_preferences(PreferenceChange {
        interface_size: Some(120),
        ..PreferenceChange::default()
    });
    assert_eq!(editor.preferences.applied().unwrap().interface_size, 120);
    answer_preference(&mut editor);
    assert!(
        editor
            .status
            .text
            .starts_with("Could not save preferences: "),
        "{}",
        editor.status.text
    );
    assert!(
        editor.status.text.contains("interface_size must be one of"),
        "{}",
        editor.status.text
    );
    assert!(editor.preferences.idle());
    assert!(editor.preferences.error.is_some());
    assert_eq!(
        editor.preferences.applied().unwrap().interface_size,
        100,
        "the stored value is back"
    );
    answer_preferences_read(&mut editor);
    assert!(editor.preferences.error.is_none());
    assert_eq!(read(&editor)["interface_size"], 100, "nothing was written");
    finish(editor, root);
}

/// Closing the window waits for the write in flight and the one waiting behind it.
#[test]
fn closing_the_window_waits_for_the_last_preference_write() {
    let (mut editor, root) = launch();
    let _ = editor.update(Message::Performance(PerformanceMessage::Toggle));
    general(
        &mut editor,
        GeneralPreference::AutoLensProfile,
        GeneralValue::Toggle(false),
    );
    let _ = editor.update(Message::Close);
    assert!(editor.preferences.closing);
    assert!(editor.owner_join.is_some(), "the owner still runs");
    answer_preference(&mut editor);
    assert!(
        editor.owner_join.is_some(),
        "the waiting change is still to be stored"
    );
    answer_preference(&mut editor);
    assert!(
        editor.owner_join.is_none(),
        "the last write landed, so it closed"
    );
    let stored = std::fs::read_to_string(root.join("config").join("preferences.json")).unwrap();
    assert!(
        stored.contains("\"performance_expanded\": false")
            && stored.contains("\"auto_lens_profile\": false"),
        "{stored}"
    );
    finish(editor, root);
}

/// The Masks panel's colour control and General's row both set this session's colour at once and
/// store the preference, which the other then shows.
#[test]
fn the_masks_panel_colour_and_the_general_row_set_the_session_and_store_the_preference() {
    let (mut editor, root) = launch();
    let _ = editor.update(Message::Mask(MaskMessage::OverlayColour(1)));
    assert_eq!(
        editor.session.workspace.mask_overlay_colour,
        MaskOverlayColour::White
    );
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"mask_overlay_colour": "white"})
    );
    answer_preference(&mut editor);
    assert_eq!(read(&editor)["mask_overlay_colour"], "white");

    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::General,
    )));
    answer_read(&mut editor);
    let row = editor.workspace.settings.general[2].clone();
    assert_eq!(row.preference, GeneralPreference::MaskOverlayColour);
    assert!(matches!(
        row.control,
        GeneralControl::Choice {
            selected: Some(1),
            ..
        }
    ));

    general(
        &mut editor,
        GeneralPreference::MaskOverlayColour,
        GeneralValue::Choice(0),
    );
    assert_eq!(
        editor.session.workspace.mask_overlay_colour,
        MaskOverlayColour::Green
    );
    assert_eq!(editor.workspace.masks.overlay.colour_selected, 0);
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"mask_overlay_colour": "green"})
    );
    answer_preference(&mut editor);
    assert_eq!(read(&editor)["mask_overlay_colour"], "green");
    finish(editor, root);
}

/// Another client's `preferences.set` is read again whether or not the sheet is open, and its mask
/// overlay colour reaches this session; an open sheet reads the flags and the preferences again.
#[test]
fn another_clients_preference_change_is_read_again_and_applies_the_mask_colour() {
    let (mut editor, root) = launch();
    let agent = editor.owner.register();
    call(
        &editor.owner,
        agent,
        "preferences.set",
        json!({"mask_overlay_colour": "white"}),
    )
    .unwrap();
    let polled = poll(&mut editor);
    assert!(polled.preferences && !polled.flags && polled.refresh.is_none());
    assert!(
        !editor.settings.reading,
        "a closed sheet reads no flags for it"
    );
    answer_preferences_read(&mut editor);
    assert_eq!(
        editor.session.workspace.mask_overlay_colour,
        MaskOverlayColour::White,
        "the agent's colour reaches this session"
    );
    assert!(!editor.preferences.reading.in_flight());

    // An open sheet reads both again and shows the change.
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::General,
    )));
    answer_read(&mut editor);
    call(
        &editor.owner,
        agent,
        "preferences.set",
        json!({"auto_lens_profile": false}),
    )
    .unwrap();
    poll(&mut editor);
    assert!(editor.settings.reading, "the open sheet reads again");
    answer_read(&mut editor);
    assert_eq!(
        editor.workspace.settings.general[1].control,
        GeneralControl::Toggle(false)
    );
    assert_eq!(
        editor.session.workspace.mask_overlay_colour,
        MaskOverlayColour::White,
        "an unchanged colour is left as it is"
    );
    finish(editor, root);
}

/// The `preference` step sends each General row's own message for a value its row does not show
/// yet and waits for the writer's last answer; a step the rows already show sends nothing; a field
/// no row shows, or a value its control does not offer, fails the step with nothing sent.
#[test]
fn a_preference_step_drives_the_general_rows_and_waits_for_the_writer() {
    let (mut editor, root) = launch();
    crate::app::testing::attach_script(
        &mut editor,
        r#"[{"preference":{"auto_lens_profile":false,"mask_overlay_colour":"white"}},
            {"preference":{"auto_lens_profile":false}},
            {"preference":{"canvas":"grey"}},
            {"preference":{"auto_lens_profile":true,"mask_overlay_colour":"purple"}}]"#,
    );
    let evidence = |editor: &Editor| {
        let evidence = editor.evidence.as_ref().expect("evidence");
        (evidence.awaiting, evidence.capture_pending)
    };
    let _ = editor.next_step();
    assert_eq!(evidence(&editor), (Some(Settle::Preferences), false));
    assert_eq!(
        editor.session.workspace.mask_overlay_colour,
        MaskOverlayColour::White
    );
    assert_eq!(
        editor.preferences.writing().unwrap().params(),
        json!({"auto_lens_profile": false})
    );
    assert_eq!(
        editor.preferences.waiting().unwrap().params(),
        json!({"mask_overlay_colour": "white"})
    );
    answer_preference(&mut editor);
    assert!(!evidence(&editor).1, "waits for the last write");
    answer_preference(&mut editor);
    assert!(evidence(&editor).1, "captured once the writer is idle");
    let applied = &editor.snapshot()["preferences"]["applied"];
    assert_eq!(
        (
            &applied["auto_lens_profile"],
            &applied["mask_overlay_colour"]
        ),
        (&json!(false), &json!("white")),
        "the frame records what the desktop applies"
    );

    // Already shown: nothing is sent and the next frame is captured.
    editor.evidence.as_mut().unwrap().capture_pending = false;
    let _ = editor.next_step();
    assert!(evidence(&editor).1);
    assert!(editor.preferences.idle());

    for expected in [
        "the General tab shows no preference canvas",
        "mask_overlay_colour's control does not offer \"purple\"",
    ] {
        editor.evidence.as_mut().unwrap().capture_pending = false;
        let _ = editor.next_step();
        assert_eq!(editor.status.text, expected);
        assert!(evidence(&editor).1, "a failed step is still captured");
        assert!(editor.preferences.idle(), "nothing is sent");
    }
    finish(editor, root);
}
