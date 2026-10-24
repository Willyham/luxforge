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
        diagnostics: None,
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
        "ui-claim".to_owned(),
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
        "ui-claim".to_owned(),
    )))));
    assert_eq!(editor.usage.prompt, Prompt::Declined);
    assert!(editor.workspace.usage_notice.is_none());
    finish(editor, root);
}

#[test]
fn usage_a_claim_that_says_no_or_fails_shows_nothing() {
    for answer in [
        Ok((false, read(true, true, true), "ui-claim".to_owned())),
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
        "ui-claim".to_owned(),
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
        "ui-claim".to_owned(),
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
        "ui-claim".to_owned(),
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
        "ui-claim".to_owned(),
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
        "ui-claim".to_owned(),
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
            SettingsTab::Agents,
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
    // No module of this launch holds a model, so the Models tab is not listed.
    let shown: Vec<SettingsTab> = SettingsTab::shown(true, false).collect();
    for tab in shown.iter().copied() {
        let _ = editor.update(Message::Settings(SettingsMessage::Open(tab)));
        assert_eq!(editor.settings.open, Some(tab));
    }
    assert_eq!(editor.workspace.settings.tabs, shown);
    assert_eq!(
        editor.settings_summary()["tabs"],
        json!(["general", "appearance", "agents", "experiments", "privacy"])
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

// ---- against a real owner ------------------------------------------------------------------------

/// The loopback receiver the test host is eligible with. Nothing listens and nothing is sent.
const RECEIVER: &str = "http://127.0.0.1:9/v1/usage/batches";

/// An editor over an owner with explicit test eligibility and preferences of its own, whose tool
/// discovery has answered.
fn launch_eligible(root: &std::path::Path) -> Editor {
    use luxforge_core::usage::{Eligibility, TestReceiver, Usage};
    let host = luxforge_core::HostConfig {
        preferences_dir: Some(root.join("config")),
        usage_consent: std::sync::Arc::new(Usage::new(Eligibility::test(
            TestReceiver::loopback(RECEIVER).unwrap(),
        ))),
        ..luxforge_core::HostConfig::unconfigured()
    };
    let (owner, join) = luxforge_core::OwnerHandle::start_with_host(
        &root.join("catalog.sqlite"),
        std::sync::Arc::new(luxforge_core::ModuleRegistry::builtin()),
        host,
    )
    .unwrap();
    let (mut editor, _) = Editor::new(super::Boot {
        owner,
        join,
        live_server: None,
        config: crate::Config {
            usage_eligible: true,
            ..crate::Config::default()
        },
        client: None,
        initial_import: None,
        window: (1440.0, 900.0),
    });
    editor.modules_ready = true;
    editor
}

fn stop(mut editor: Editor) {
    editor.owner.stop();
    if let Some(join) = editor.owner_join.take() {
        join.join().unwrap();
    }
}

/// Answer the claim the editor started, as its task would.
fn answer_claim(editor: &mut Editor) {
    assert_eq!(editor.usage.prompt, Prompt::Claiming, "a claim is out");
    let answer = super::usage::calls::claim(&editor.owner, editor.client);
    let _ = editor.update(Message::Usage(UsageMessage::Claimed(answer)));
}

/// Answer the write in flight, as its task would.
fn answer_set(editor: &mut Editor) {
    let enabled = editor.usage.writing.expect("a write in flight");
    let answer = super::usage::calls::set(&editor.owner, editor.client, enabled);
    let _ = editor.update(Message::Usage(UsageMessage::Saved(answer)));
}

fn answer_read(editor: &mut Editor) {
    assert!(editor.usage.reading, "a read is out");
    let answer = super::usage::calls::read(&editor.owner, editor.client);
    let _ = editor.update(Message::Usage(UsageMessage::Read(answer)));
}

#[test]
fn usage_the_real_claim_shows_the_notice_once_over_retained_preferences() {
    let root = luxforge_testbase::paths::temp_path("usage-claim-once");
    let mut editor = launch_eligible(&root);
    nudge(&mut editor);
    answer_claim(&mut editor);
    assert_eq!(editor.usage.prompt, Prompt::Shown);
    let summary = editor.usage_summary();
    assert_eq!(
        summary["stored"]["prompt_shown"], true,
        "claimed before shown"
    );
    assert_eq!(summary["stored"]["enabled"], false);
    assert_eq!(summary["effective"], false);
    assert_eq!(summary["destination"], RECEIVER);
    // A second claim in the same process, as a retry would make, is told not to show it.
    let (again, _, _) = super::usage::calls::claim(&editor.owner, editor.client).unwrap();
    assert!(!again);
    // Escape refuses: nothing is written, and the record keeps sharing off.
    escape(&mut editor);
    assert_eq!(editor.usage.prompt, Prompt::Declined);
    let stored = super::usage::calls::read(&editor.owner, editor.client).unwrap();
    assert_eq!(
        (stored.prompt_shown, stored.enabled, stored.effective),
        (Some(true), Some(false), false)
    );
    stop(editor);

    // The next launch over the same preferences claims nothing to show.
    let mut editor = launch_eligible(&root);
    nudge(&mut editor);
    answer_claim(&mut editor);
    assert_eq!(editor.usage.prompt, Prompt::NotShown);
    assert!(editor.workspace.usage_notice.is_none());
    stop(editor);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn usage_share_and_the_privacy_switch_store_through_usage_set_and_another_client_is_followed() {
    let root = luxforge_testbase::paths::temp_path("usage-share");
    let mut editor = launch_eligible(&root);
    nudge(&mut editor);
    answer_claim(&mut editor);
    let _ = editor.update(Message::Usage(UsageMessage::Share));
    answer_set(&mut editor);
    let summary = editor.usage_summary();
    assert_eq!(summary["effective"], true, "{summary}");
    assert_eq!(summary["stored"]["enabled"], true);
    assert!(
        !summary.to_string().contains("installation"),
        "no identifier is recorded: {summary}"
    );
    let stored = std::fs::read_to_string(root.join("config").join("preferences.json")).unwrap();
    assert!(
        stored.contains("installation_id"),
        "the opt-in created its ID"
    );

    // The Privacy tab shows it, and turns it off.
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Privacy,
    )));
    answer_read(&mut editor);
    assert!(editor.workspace.settings.privacy.as_ref().unwrap().on);
    let _ = editor.update(Message::Usage(UsageMessage::Set(false)));
    answer_set(&mut editor);
    let privacy = editor.workspace.settings.privacy.clone().unwrap();
    assert!(!privacy.on && privacy.error.is_none());
    let stored = std::fs::read_to_string(root.join("config").join("preferences.json")).unwrap();
    assert!(
        !stored.contains("installation_id"),
        "the opt-out removed it"
    );
    let _ = editor.update(Message::Usage(UsageMessage::Set(true)));
    answer_set(&mut editor);
    assert!(editor.workspace.settings.privacy.as_ref().unwrap().on);

    // Another client opts out; the event sync sees a usage event that is not the desktop's own,
    // and the open tab reads it again.
    let agent = editor.owner.register();
    super::tasks::call(
        &editor.owner,
        agent,
        "usage.set",
        json!({"enabled": false, "mutation": {"request_id": "agent-off", "actor": "agent"}}),
    )
    .unwrap();
    let polled = super::tasks::sync_now(
        &editor.owner,
        editor.client,
        None,
        editor.sync.sequence,
        &editor.sync.own_requests.iter().cloned().collect::<Vec<_>>(),
        None,
    )
    .unwrap();
    assert!(polled.usage, "another client's consent change is seen");
    let _ = editor.settings_changed_elsewhere(false, false, false, polled.usage);
    answer_read(&mut editor);
    let privacy = editor.workspace.settings.privacy.clone().unwrap();
    assert!(!privacy.on);
    assert_eq!(editor.usage_summary()["stored"]["enabled"], false);
    stop(editor);
    std::fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn usage_a_failed_opt_out_leaves_this_process_off_and_says_the_stored_choice_stayed() {
    use std::os::unix::fs::PermissionsExt;
    let root = luxforge_testbase::paths::temp_path("usage-failed-off");
    let mut editor = launch_eligible(&root);
    nudge(&mut editor);
    answer_claim(&mut editor);
    let _ = editor.update(Message::Usage(UsageMessage::Share));
    answer_set(&mut editor);
    assert_eq!(editor.usage_summary()["effective"], true);
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::Privacy,
    )));
    answer_read(&mut editor);

    // The preferences can no longer be written.
    let config = root.join("config");
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o500)).unwrap();
    let _ = editor.update(Message::Usage(UsageMessage::Set(false)));
    answer_set(&mut editor);
    answer_read(&mut editor);
    std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o700)).unwrap();
    let summary = editor.usage_summary();
    assert_eq!(
        summary["effective"], false,
        "this process is off: {summary}"
    );
    assert_eq!(
        summary["stored"]["enabled"], true,
        "the stored choice stayed"
    );
    let privacy = editor.workspace.settings.privacy.clone().unwrap();
    assert!(!privacy.on);
    assert!(
        privacy
            .error
            .as_deref()
            .is_some_and(|error| error.starts_with(
                "Usage statistics are off for this session, but the stored choice did not change:"
            )),
        "{:?}",
        privacy.error
    );
    stop(editor);
    std::fs::remove_dir_all(root).unwrap();
}
