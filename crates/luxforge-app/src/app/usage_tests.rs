//! The usage statistics notice and the Privacy tab's lifecycle on the desktop: one claim once the
//! workspace is usable, a claimed notice held back while something delays it, its two answers and
//! Escape, the switch's writes and their failures, another client's change, and both surfaces
//! hidden in a launch that is not eligible. Owner answers are handed to the update function as
//! their tasks would hand them.
use super::{
    Editor, keymap,
    message::{Message, settings::SettingsMessage, usage::UsageMessage},
    settings_tests::{finish, launch},
};
use crate::state::{
    settings::SettingsTab,
    usage::{Prompt, UsageRead},
};
use iced::{
    Event,
    event::Status,
    keyboard::{self, Key, Modifiers, key::Named},
};
use serde_json::json;

/// The consent as the core reports it.
fn read(prompt_shown: bool, enabled: bool, effective: bool) -> UsageRead {
    UsageRead {
        prompt_shown: Some(prompt_shown),
        enabled: Some(enabled),
        effective,
        disclosure_version: 1,
        destination: None,
        reason: (!effective).then(|| "choice".into()),
    }
}

/// An editor launched eligible, whose tool discovery has answered.
fn eligible() -> (Editor, std::path::PathBuf) {
    let (mut editor, root) = launch();
    editor.usage.eligible = true;
    editor.modules_ready = true;
    (editor, root)
}

/// A message that changes nothing but runs every hook.
fn nudge(editor: &mut Editor) {
    let _ = editor.update(Message::Usage(UsageMessage::Details(
        editor.usage.details_open,
    )));
}

fn escape(editor: &mut Editor) {
    let event = Event::Keyboard(keyboard::Event::KeyPressed {
        key: Key::Named(Named::Escape),
        modified_key: Key::Named(Named::Escape),
        physical_key: keyboard::key::Physical::Unidentified(
            keyboard::key::NativeCode::Unidentified,
        ),
        location: keyboard::Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat: false,
    });
    if let Some(message) = keymap::keymap(&event, Status::Ignored, &editor.key_context()) {
        let _ = editor.update(message);
    }
}

#[test]
fn usage_the_notice_is_claimed_once_after_the_workspace_is_usable() {
    let (mut editor, root) = launch();
    editor.usage.eligible = true;
    nudge(&mut editor);
    assert_eq!(
        editor.usage.prompt,
        Prompt::Waiting,
        "nothing is claimed before tool discovery answers"
    );
    editor.modules_ready = true;
    nudge(&mut editor);
    assert_eq!(editor.usage.prompt, Prompt::Claiming);
    // Every later message finds the claim out and sends no other.
    for _ in 0..3 {
        nudge(&mut editor);
        assert_eq!(editor.usage.prompt, Prompt::Claiming);
    }
    let _ = editor.update(Message::Usage(UsageMessage::Claimed(Ok((
        true,
        read(true, false, false),
    )))));
    assert_eq!(editor.usage.prompt, Prompt::Shown, "nothing delays it");
    let notice = editor.workspace.usage_notice.clone().expect("drawn");
    assert_eq!(notice.title, "Help improve Luxforge?");
    assert_eq!(notice.actions, ["Share statistics", "No thanks"]);
    // A duplicate answer, as a retried claim's, never shows it a second time.
    let _ = editor.update(Message::Usage(UsageMessage::Decline));
    let _ = editor.update(Message::Usage(UsageMessage::Claimed(Ok((
        true,
        read(true, false, false),
    )))));
    assert_eq!(editor.usage.prompt, Prompt::Declined);
    assert!(editor.workspace.usage_notice.is_none());
    finish(editor, root);
}

#[test]
fn usage_a_claim_that_says_no_or_fails_shows_nothing() {
    for answer in [
        Ok((false, read(true, true, true))),
        Err("the preferences file is read-only".to_owned()),
    ] {
        let (mut editor, root) = eligible();
        nudge(&mut editor);
        let failed = answer.is_err();
        let _ = editor.update(Message::Usage(UsageMessage::Claimed(answer)));
        assert!(editor.workspace.usage_notice.is_none());
        assert_eq!(
            editor.usage_summary()["prompt"],
            if failed { "failed" } else { "not-shown" }
        );
        nudge(&mut editor);
        assert!(
            !matches!(editor.usage.prompt, Prompt::Claiming),
            "never claimed again"
        );
        finish(editor, root);
    }
}

#[test]
fn usage_a_claimed_notice_waits_for_a_native_dialog_a_consent_notice_or_the_sheet() {
    let (mut editor, root) = eligible();
    nudge(&mut editor);
    editor.view_state.picker_open = true;
    let _ = editor.update(Message::Usage(UsageMessage::Claimed(Ok((
        true,
        read(true, false, false),
    )))));
    assert_eq!(editor.usage.prompt, Prompt::Pending);
    assert!(editor.workspace.usage_notice.is_none());
    editor.view_state.picker_open = false;
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::General,
    )));
    assert_eq!(
        editor.usage.prompt,
        Prompt::Pending,
        "the sheet holds it back"
    );
    let _ = editor.update(Message::Settings(SettingsMessage::Close));
    assert_eq!(editor.usage.prompt, Prompt::Shown);
    assert!(editor.workspace.usage_notice.is_some());
    finish(editor, root);
}

#[test]
fn usage_escape_and_no_thanks_send_nothing_and_share_writes_the_opt_in() {
    let (mut editor, root) = eligible();
    nudge(&mut editor);
    let _ = editor.update(Message::Usage(UsageMessage::Claimed(Ok((
        true,
        read(true, false, false),
    )))));
    escape(&mut editor);
    assert_eq!(editor.usage.prompt, Prompt::Declined);
    assert_eq!(
        editor.usage.writes_outstanding(),
        0,
        "refusing writes nothing"
    );
    assert!(editor.workspace.usage_notice.is_none());
    // Share on a notice no longer shown does nothing.
    let _ = editor.update(Message::Usage(UsageMessage::Share));
    assert_eq!(editor.usage.writes_outstanding(), 0);
    finish(editor, root);

    let (mut editor, root) = eligible();
    nudge(&mut editor);
    let _ = editor.update(Message::Usage(UsageMessage::Claimed(Ok((
        true,
        read(true, false, false),
    )))));
    let _ = editor.update(Message::Usage(UsageMessage::Share));
    assert_eq!(editor.usage.prompt, Prompt::Shared);
    assert_eq!(
        editor.usage.writing,
        Some(true),
        "one usage.set asking for on"
    );
    assert!(editor.workspace.usage_notice.is_none());
    let _ = editor.update(Message::Usage(UsageMessage::Saved(Ok((
        read(true, true, true),
        "ui-share".into(),
    )))));
    let summary = editor.usage_summary();
    assert_eq!(summary["effective"], true);
    assert_eq!(summary["stored"]["enabled"], true);
    assert_eq!(summary["writes_outstanding"], 0);
    assert!(editor.sync.own_requests.contains(&"ui-share".to_owned()));
    finish(editor, root);
}

#[test]
fn usage_the_opt_in_body_names_the_disclosure_and_the_opt_out_names_none() {
    let on = super::usage::calls::set_params(true, "ui-1");
    assert_eq!(on["enabled"], true);
    assert_eq!(
        on["disclosure_version"],
        luxforge_core::usage::DISCLOSURE_VERSION
    );
    assert_eq!(on["mutation"]["request_id"], "ui-1");
    let off = super::usage::calls::set_params(false, "ui-2");
    assert_eq!(off["enabled"], false);
    assert!(off.get("disclosure_version").is_none());
}

#[test]
fn usage_the_privacy_switch_writes_in_order_and_says_what_a_failure_left() {
    let (mut editor, root) = eligible();
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Privacy,
    )));
    assert_eq!(editor.settings.open, Some(SettingsTab::Privacy));
    assert!(editor.usage.reading, "opening reads the consent");
    let _ = editor.update(Message::Usage(UsageMessage::Read(Ok(read(
        true, true, true,
    )))));
    assert!(editor.workspace.settings.privacy.as_ref().unwrap().on);

    // Off shows at once; a change made meanwhile waits behind it.
    let _ = editor.update(Message::Usage(UsageMessage::Set(false)));
    let privacy = editor.workspace.settings.privacy.clone().unwrap();
    assert!(!privacy.on && privacy.saving);
    let _ = editor.update(Message::Usage(UsageMessage::Set(true)));
    assert_eq!(editor.usage.writing, Some(false));
    assert_eq!(editor.usage.waiting, Some(true));
    assert_eq!(editor.usage_summary()["writes_outstanding"], 2);

    // The opt-out fails: this process is off, the stored choice is not changed, and the tab says
    // so. The waiting opt-in is sent next.
    let _ = editor.update(Message::Usage(UsageMessage::Saved(Err(
        "the preferences file is read-only".into(),
    ))));
    assert_eq!(editor.usage.writing, Some(true));
    assert!(editor.usage.reading, "the truth is read back");
    let _ = editor.update(Message::Usage(UsageMessage::Read(Ok(read(
        true, true, false,
    )))));
    let privacy = editor.workspace.settings.privacy.clone().unwrap();
    assert!(!privacy.on, "an opt-in shows only once saved");
    assert_eq!(
        privacy.error.as_deref(),
        Some(
            "Usage statistics are off for this session, but the stored choice did not change: \
             the preferences file is read-only"
        )
    );
    assert_eq!(
        editor.status.text,
        "Usage statistics are off for this session, but the stored choice did not change: the \
         preferences file is read-only"
    );
    // The opt-in fails too, and says so.
    let _ = editor.update(Message::Usage(UsageMessage::Saved(Err(
        "permission authority required".into(),
    ))));
    let _ = editor.update(Message::Usage(UsageMessage::Read(Ok(read(
        true, true, false,
    )))));
    let privacy = editor.workspace.settings.privacy.clone().unwrap();
    assert!(!privacy.on && !privacy.saving);
    assert_eq!(
        privacy.error.as_deref(),
        Some("Could not turn on usage statistics: permission authority required")
    );
    // A later success clears the failure.
    let _ = editor.update(Message::Usage(UsageMessage::Set(false)));
    let _ = editor.update(Message::Usage(UsageMessage::Saved(Ok((
        read(true, false, false),
        "ui-off".into(),
    )))));
    let privacy = editor.workspace.settings.privacy.clone().unwrap();
    assert!(!privacy.on && privacy.error.is_none());
    finish(editor, root);
}

#[test]
fn usage_a_choice_in_privacy_before_the_notice_appears_answers_it() {
    let (mut editor, root) = eligible();
    nudge(&mut editor);
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Privacy,
    )));
    let _ = editor.update(Message::Usage(UsageMessage::Claimed(Ok((
        true,
        read(true, false, false),
    )))));
    assert_eq!(editor.usage.prompt, Prompt::Pending, "the sheet holds it");
    let _ = editor.update(Message::Usage(UsageMessage::Set(true)));
    let _ = editor.update(Message::Usage(UsageMessage::Saved(Ok((
        read(true, true, true),
        "ui-on".into(),
    )))));
    let _ = editor.update(Message::Settings(SettingsMessage::Close));
    assert_eq!(editor.usage.prompt, Prompt::Superseded);
    assert!(editor.workspace.usage_notice.is_none());
    finish(editor, root);
}

#[test]
fn usage_an_open_privacy_tab_follows_another_clients_change() {
    let (mut editor, root) = eligible();
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Privacy,
    )));
    let _ = editor.update(Message::Usage(UsageMessage::Read(Ok(read(
        true, true, true,
    )))));
    assert!(!editor.usage.reading);
    // The event sync saw a `usage.*` event of another client's.
    let _ = editor.settings_changed_elsewhere(false, false, false, true);
    assert!(editor.usage.reading);
    let _ = editor.update(Message::Usage(UsageMessage::Read(Ok(read(
        true, false, false,
    )))));
    let privacy = editor.workspace.settings.privacy.clone().unwrap();
    assert!(!privacy.on && privacy.error.is_none());
    assert_eq!(editor.usage_summary()["stored"]["enabled"], false);
    finish(editor, root);
}

#[test]
fn usage_an_ineligible_launch_shows_neither_the_notice_nor_the_privacy_tab() {
    let (mut editor, root) = launch();
    editor.modules_ready = true;
    nudge(&mut editor);
    assert_eq!(editor.usage.prompt, Prompt::Waiting, "never claimed");
    let _ = editor.update(Message::Usage(UsageMessage::Claimed(Ok((
        true,
        read(true, false, false),
    )))));
    assert!(editor.workspace.usage_notice.is_none());
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Privacy,
    )));
    assert_eq!(
        editor.settings.open,
        Some(SettingsTab::General),
        "no Privacy tab to open"
    );
    assert!(!editor.usage.reading, "nothing is read");
    assert_eq!(
        editor.workspace.settings.tabs,
        [
            SettingsTab::General,
            SettingsTab::Appearance,
            SettingsTab::Experiments
        ]
    );
    assert!(editor.workspace.settings.privacy.is_none());
    assert!(
        !editor
            .workspace
            .palette
            .entries
            .iter()
            .any(|entry| entry.label.contains("Privacy"))
    );
    let summary = editor.usage_summary();
    assert_eq!(summary["eligible"], false);
    assert_eq!(summary["privacy"], json!(null));
    finish(editor, root);
}

#[test]
fn usage_settings_lists_privacy_last_in_an_eligible_launch_and_keeps_the_other_tabs() {
    let (mut editor, root) = eligible();
    for tab in SettingsTab::ALL {
        let _ = editor.update(Message::Settings(SettingsMessage::Open(tab)));
        assert_eq!(editor.settings.open, Some(tab));
    }
    assert_eq!(editor.workspace.settings.tabs, SettingsTab::ALL);
    assert_eq!(
        editor.settings_summary()["tabs"],
        json!(["general", "appearance", "experiments", "privacy"])
    );
    let _ = editor.update(Message::Usage(UsageMessage::Details(true)));
    assert!(
        editor
            .workspace
            .settings
            .privacy
            .as_ref()
            .unwrap()
            .details_open
    );
    finish(editor, root);
}
