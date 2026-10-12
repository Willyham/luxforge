//! Automatic updates on the desktop against a real owner and a scripted update port: the offer
//! appears for an available, unskipped release and follows its download, Later hides it until the
//! next check, Skip and Install send their commands and show what the owner answers, a refused
//! install says why, a release that changes the catalog format says so, a notify-only Linux
//! installation offers Download, the Settings group says why updates are unavailable, its switches
//! round-trip through `preferences.set`, and the palette's Check for Updates runs `update.check`.
use super::{
    Editor,
    message::{Message, palette::PaletteMessage, settings::SettingsMessage, update::UpdateMessage},
    settings_tests::{answer_preference, answer_read, finish},
    tasks::{call, call_own},
    update::calls,
};
use crate::state::{
    palette::{CHECK_FOR_UPDATES, PaletteAction},
    settings::SettingsTab,
    update::{NEW_CATALOG, NoticeButton, notice},
};
use luxforge_core::{
    Error, ErrorKind, HostConfig, ModuleRegistry, OwnerHandle,
    update::{
        CATALOG_FORMAT, FeedUpdate, Progress, UnavailablePort, UnavailableReason, UpdatePort,
        Updater, testing::FakeUpdatePort,
    },
};
use serde_json::json;
use std::{path::PathBuf, sync::Arc};

/// An editor whose host has `port`, with its preferences in a directory of its own. The launch's
/// read of the update status is still in flight.
fn launch(port: Arc<dyn UpdatePort>) -> (Editor, PathBuf) {
    let root = luxforge_testbase::paths::temp_path("desktop-updates");
    let host = HostConfig {
        preferences_dir: Some(root.join("config")),
        updates: Some(port),
        ..HostConfig::unconfigured()
    };
    let (owner, join) = OwnerHandle::start_with_host(
        &root.join("catalog.sqlite"),
        Arc::new(ModuleRegistry::developer()),
        host,
    )
    .unwrap();
    let (editor, _) = Editor::new(super::Boot {
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

/// A Sparkle port and an editor over it, its launch read answered.
fn sparkle() -> (Arc<FakeUpdatePort>, Editor, PathBuf) {
    let port = FakeUpdatePort::new(Updater::Sparkle);
    let (mut editor, root) = launch(port.clone());
    answer_status(&mut editor);
    (port, editor, root)
}

/// Answer the update status read in flight as its task would.
fn answer_status(editor: &mut Editor) {
    assert!(
        editor.updates.reading.in_flight(),
        "a status read in flight"
    );
    let answer = calls::status(&editor.owner, editor.client);
    let _ = editor.update(Message::Update(UpdateMessage::Read(answer)));
}

/// Read the status again, as the event sync does for an `update.status` event, and answer it.
fn reread(editor: &mut Editor) {
    let _ = editor.read_updates();
    answer_status(editor);
}

/// What `port` reports from now on: `release` available, and nothing checked yet.
fn offers(port: &FakeUpdatePort, release: FeedUpdate) {
    port.update(|status| status.available = Some(release));
}

fn press(editor: &mut Editor, button: NoticeButton) {
    let _ = editor.update(Message::Update(UpdateMessage::Notice(button)));
}

/// Answer the check in flight as its task would.
fn answer_check(editor: &mut Editor) {
    assert!(editor.updates.checking, "a check in flight");
    let answer = calls::check(&editor.owner, editor.client);
    let _ = editor.update(Message::Update(UpdateMessage::Checked(answer)));
}

/// Answer the install in flight as its task would.
fn answer_install(editor: &mut Editor) {
    assert!(editor.updates.installing, "an install in flight");
    let answer = calls::install(&editor.owner, editor.client);
    let _ = editor.update(Message::Update(UpdateMessage::Installed(answer)));
}

/// An offer appears for an available release, with its title and buttons, and shows the download
/// as it runs and once it is ready; Later hides it until a check ends.
#[test]
fn the_offer_follows_the_release_its_download_and_later() {
    let (port, mut editor, root) = sparkle();
    assert!(
        editor.workspace.update_notice.is_none(),
        "nothing available, nothing offered"
    );

    offers(&port, FakeUpdatePort::release("0.2.0", 2));
    reread(&mut editor);
    let offer = editor.workspace.update_notice.clone().expect("an offer");
    assert_eq!(offer.title, "Luxforge 0.2.0 is available");
    assert_eq!(
        offer.buttons,
        [
            NoticeButton::ReleaseNotes,
            NoticeButton::Install,
            NoticeButton::Later,
            NoticeButton::Skip
        ]
    );
    assert_eq!(
        offer.url.as_deref(),
        Some("https://github.com/luxforge/luxforge/releases/tag/v0.2.0")
    );
    assert!(offer.lines.is_empty(), "{:?}", offer.lines);
    assert_eq!(offer.progress, None);
    assert_eq!(
        editor.updates.shown.as_ref().map(|shown| shown.0.as_str()),
        Some("Luxforge 0.2.0 is available"),
        "the offer is recorded when it appears"
    );

    port.update(|status| {
        status.progress = Some(Progress {
            received: 25_000_000,
            total: Some(100_000_000),
        });
    });
    reread(&mut editor);
    let offer = editor.workspace.update_notice.clone().unwrap();
    assert_eq!(offer.lines, ["Downloading\u{2026} 25%"]);
    assert_eq!(offer.progress, Some(Some(0.25)));

    port.update(|status| {
        status.progress = None;
        status.ready = true;
    });
    reread(&mut editor);
    let offer = editor.workspace.update_notice.clone().unwrap();
    assert_eq!(offer.lines, ["Downloaded and ready to install."]);
    assert_eq!(offer.progress, None);

    // Release notes records the page; no browser opens in a test.
    press(&mut editor, NoticeButton::ReleaseNotes);
    assert_eq!(
        editor.updates.opened.as_deref(),
        Some("https://github.com/luxforge/luxforge/releases/tag/v0.2.0")
    );

    // Later sends nothing and hides the offer, through a re-read, until a check ends.
    let applied = port.applied();
    press(&mut editor, NoticeButton::Later);
    assert!(editor.workspace.update_notice.is_none(), "Later hides it");
    reread(&mut editor);
    assert!(editor.workspace.update_notice.is_none(), "still hidden");
    assert_eq!(
        (port.installs(), port.skips().len(), port.applied()),
        (0, 0, applied),
        "Later sent no command"
    );
    port.finds(Some(FakeUpdatePort::release("0.2.0", 2)));
    let _ = editor.update(Message::Update(UpdateMessage::Check));
    assert!(
        editor.workspace.update_notice.is_none(),
        "a check in flight does not bring it back"
    );
    answer_check(&mut editor);
    assert_eq!(port.checks(), 1);
    let offer = editor
        .workspace
        .update_notice
        .clone()
        .expect("back after the check");
    assert_eq!(offer.title, "Luxforge 0.2.0 is available");
    finish(editor, root);
}

/// Skip this version sends `update.skip` of the version on offer and the offer goes; a later
/// release is offered again.
#[test]
fn skip_sends_update_skip_and_the_offer_goes() {
    let (port, mut editor, root) = sparkle();
    offers(&port, FakeUpdatePort::release("0.2.0", 2));
    reread(&mut editor);
    press(&mut editor, NoticeButton::Skip);
    assert_eq!(editor.updates.skipping.as_deref(), Some("0.2.0"));
    assert!(
        editor.workspace.update_notice.is_none(),
        "the offer goes while the skip is answered"
    );
    let answer = calls::skip(&editor.owner, editor.client, "0.2.0");
    let _ = editor.update(Message::Update(UpdateMessage::Skipped(answer)));
    assert_eq!(port.skips(), ["0.2.0"]);
    assert_eq!(editor.updates.skipping, None);
    let status = editor.updates.status.as_ref().unwrap();
    assert!(status.available.as_ref().unwrap().skipped);
    assert!(editor.workspace.update_notice.is_none(), "skipped");
    reread(&mut editor);
    assert!(editor.workspace.update_notice.is_none(), "still skipped");

    offers(&port, FakeUpdatePort::release("0.3.0", 3));
    reread(&mut editor);
    assert_eq!(
        editor
            .workspace
            .update_notice
            .as_ref()
            .map(|offer| offer.title.as_str()),
        Some("Luxforge 0.3.0 is available")
    );
    finish(editor, root);
}

/// Install and relaunch sends `update.install`; while it is answered the buttons that send wait,
/// and the accepted install says Luxforge relaunches.
#[test]
fn install_sends_update_install_and_says_it_relaunches() {
    let (port, mut editor, root) = sparkle();
    offers(&port, FakeUpdatePort::release("0.2.0", 2));
    port.update(|status| status.ready = true);
    reread(&mut editor);
    press(&mut editor, NoticeButton::Install);
    assert!(editor.workspace.update_notice.as_ref().unwrap().busy);
    // A second press while the first is answered sends nothing.
    press(&mut editor, NoticeButton::Install);
    answer_install(&mut editor);
    assert_eq!(port.installs(), 1);
    let offer = editor.workspace.update_notice.clone().unwrap();
    assert!(!offer.busy);
    assert_eq!(
        offer.lines,
        ["Installing Luxforge 0.2.0. Luxforge quits and relaunches when it is ready."]
    );
    assert_eq!(offer.buttons, [NoticeButton::ReleaseNotes]);
    assert_eq!(offer.error, None);
    assert_eq!(editor.status.text, "Installing Luxforge 0.2.0\u{2026}");
    finish(editor, root);
}

/// A port's `conflict` refusal names the running work by kind; the offer stays, its buttons live.
#[test]
fn a_conflicting_install_names_the_running_work() {
    let (port, mut editor, root) = sparkle();
    offers(&port, FakeUpdatePort::release("0.2.0", 2));
    reread(&mut editor);
    port.refuse_next(
        Error::new(
            ErrorKind::Conflict,
            "installing an update waits for running jobs",
        )
        .with_data(json!({"jobs": [
            {"job_id": "job-1", "kind": "export"},
            {"job_id": "job-2", "kind": "hdr-merge"},
        ]})),
    );
    press(&mut editor, NoticeButton::Install);
    answer_install(&mut editor);
    let offer = editor.workspace.update_notice.clone().unwrap();
    let words = "Installing waits until this work finishes: an export, an HDR merge.";
    assert_eq!(offer.error.as_deref(), Some(words));
    assert_eq!(editor.status.text, words);
    assert!(!offer.busy);
    assert!(offer.buttons.contains(&NoticeButton::Install));
    // Pressing again clears the refusal while the next answer is awaited.
    press(&mut editor, NoticeButton::Install);
    assert_eq!(editor.workspace.update_notice.as_ref().unwrap().error, None);
    answer_install(&mut editor);
    assert_eq!(port.installs(), 1, "the second install was accepted");
    finish(editor, root);
}

/// A release that reads another catalog format says it needs a new catalog, the current one stays
/// and this version keeps opening it.
#[test]
fn a_catalog_format_change_says_a_new_catalog_is_needed() {
    let (port, mut editor, root) = sparkle();
    offers(
        &port,
        FeedUpdate {
            catalog_format: CATALOG_FORMAT + 1,
            ..FakeUpdatePort::release("0.2.0", 2)
        },
    );
    port.update(|status| status.ready = true);
    reread(&mut editor);
    let offer = editor.workspace.update_notice.clone().unwrap();
    assert_eq!(
        offer.lines,
        [NEW_CATALOG, "Downloaded and ready to install."]
    );
    assert!(
        NEW_CATALOG.contains("needs a new catalog")
            && NEW_CATALOG.contains("current catalog stays as it is")
            && NEW_CATALOG.contains("the version you have now keeps opening it"),
        "{NEW_CATALOG}"
    );
    assert!(offer.buttons.contains(&NoticeButton::Install));
    finish(editor, root);
}

/// A notify-only Linux installation offers Download, which opens the release page, and no
/// Install; the owner refuses an install there anyway.
#[test]
fn a_notify_only_installation_offers_download() {
    let port = FakeUpdatePort::new(Updater::LinuxNotify);
    let (mut editor, root) = launch(port.clone());
    answer_status(&mut editor);
    offers(&port, FakeUpdatePort::release("0.2.0", 2));
    reread(&mut editor);
    let offer = editor.workspace.update_notice.clone().unwrap();
    assert_eq!(
        offer.buttons,
        [
            NoticeButton::Download,
            NoticeButton::Later,
            NoticeButton::Skip
        ]
    );
    assert_eq!(
        offer.lines,
        [
            "This installation cannot replace itself. Download the new version from its release \
          page."
        ]
    );
    press(&mut editor, NoticeButton::Download);
    assert_eq!(
        editor.updates.opened.as_deref(),
        Some("https://github.com/luxforge/luxforge/releases/tag/v0.2.0")
    );
    // Install is not offered; a press of a button the offer does not show sends nothing.
    press(&mut editor, NoticeButton::Install);
    assert!(!editor.updates.installing);
    let refused = calls::install(&editor.owner, editor.client).unwrap_err();
    assert_eq!(refused.code, "not-ready");
    assert_eq!(port.installs(), 0);
    finish(editor, root);
}

/// The Settings group says in plain words why updates are unavailable, and shows no controls; an
/// unknown reason has a generic sentence.
#[test]
fn the_settings_group_says_why_updates_are_unavailable() {
    let words = [
        (
            UnavailableReason::DevelopmentBuild,
            "This development build does not update itself.",
        ),
        (
            UnavailableReason::OutsideApplications,
            "Luxforge can only update itself from the Applications folder. Move it there to get \
             updates.",
        ),
    ];
    for (reason, sentence) in words {
        let (mut editor, root) = launch(Arc::new(UnavailablePort::new(reason)));
        let _ = editor.update(Message::Settings(SettingsMessage::Open(
            SettingsTab::General,
        )));
        answer_read(&mut editor);
        answer_status(&mut editor);
        let group = &editor.workspace.settings.updates;
        assert_eq!(group.unavailable.as_deref(), Some(sentence), "{reason:?}");
        assert!(!group.loading && !group.checks && !group.install_enabled);
        assert!(editor.workspace.update_notice.is_none());
        finish(editor, root);
    }
    for reason in [
        UnavailableReason::DevelopmentBuild,
        UnavailableReason::Headless,
        UnavailableReason::NoUpdateKey,
        UnavailableReason::OutsideApplications,
        UnavailableReason::ReadOnlyInstall,
    ] {
        let text = crate::state::update::reason_text(Some(reason.as_str()));
        assert!(
            !text.contains(reason.as_str()),
            "{reason:?} has its own words: {text}"
        );
    }
    assert_eq!(
        crate::state::update::reason_text(Some("a-new-reason")),
        "Updates are unavailable for this installation (a-new-reason)."
    );
}

/// The group shows the version, the switches as stored and the last check; each switch writes
/// its preference through `preferences.set`, the install switch waits while checks are off, and
/// Check now runs `update.check` and shows when it ended and what it found.
#[test]
fn the_switches_round_trip_through_preferences_set() {
    let (port, mut editor, root) = sparkle();
    let _ = editor.update(Message::Settings(SettingsMessage::Open(
        SettingsTab::General,
    )));
    answer_read(&mut editor);
    answer_status(&mut editor);
    let group = editor.workspace.settings.updates.clone();
    assert_eq!(
        group.version,
        format!(
            "Luxforge {} (build 1)",
            luxforge_core::update::CURRENT_VERSION
        )
    );
    assert!(group.checks && !group.install_automatically && group.install_enabled);
    assert_eq!(group.last_check, "Not checked yet");

    let _ = editor.update(Message::Update(UpdateMessage::SetInstallAutomatically(
        true,
    )));
    assert!(editor.workspace.settings.updates.install_automatically);
    answer_preference(&mut editor);
    assert_eq!(
        call(&editor.owner, editor.client, "preferences.read", json!({}))
            .unwrap()
            .0["update_install_automatically"],
        true
    );
    answer_status(&mut editor);
    assert!(
        editor
            .updates
            .status
            .as_ref()
            .unwrap()
            .install_automatically
    );
    assert!(port.policy().unwrap().install_automatically);

    let _ = editor.update(Message::Update(UpdateMessage::SetChecks(false)));
    let group = &editor.workspace.settings.updates;
    assert!(!group.checks && !group.install_enabled, "off at once");
    answer_preference(&mut editor);
    answer_status(&mut editor);
    assert_eq!(
        call(&editor.owner, editor.client, "preferences.read", json!({}))
            .unwrap()
            .0["update_checks"],
        false
    );
    assert!(!editor.updates.status.as_ref().unwrap().checks);
    // The install switch is disabled now: a gesture there sends nothing.
    let _ = editor.update(Message::Update(UpdateMessage::SetInstallAutomatically(
        false,
    )));
    assert!(editor.preferences.writing().is_none(), "nothing written");

    // Check now runs a check even with checks off, and says when it ended and what it found.
    port.finds(Some(FakeUpdatePort::release("0.2.0", 2)));
    let _ = editor.update(Message::Update(UpdateMessage::Check));
    assert!(editor.workspace.settings.updates.checking);
    answer_check(&mut editor);
    let group = &editor.workspace.settings.updates;
    assert!(!group.checking);
    assert!(
        group.last_check.ends_with(": found Luxforge 0.2.0"),
        "{}",
        group.last_check
    );
    assert_eq!(port.checks(), 1);
    finish(editor, root);
}

/// The palette lists Check for Updates, which runs `update.check` through the same message as
/// Check now, and the status bar says what it found.
#[test]
fn the_palette_checks_for_updates() {
    let (port, mut editor, root) = sparkle();
    port.finds(None);
    let _ = editor.update(Message::Palette(PaletteMessage::Open));
    let _ = editor.update(Message::Palette(PaletteMessage::Query(
        "check for updates".into(),
    )));
    let entry = editor
        .workspace
        .palette
        .entries
        .first()
        .cloned()
        .expect("an entry");
    assert_eq!(entry.label, CHECK_FOR_UPDATES);
    assert_eq!(entry.detail, "update.check");
    assert_eq!(entry.action, PaletteAction::CheckForUpdates);
    let _ = editor.update(Message::Palette(PaletteMessage::Run));
    assert!(!editor.workspace.palette.open);
    assert!(editor.updates.checking);
    assert_eq!(editor.status.text, crate::state::update::CHECKING);
    answer_check(&mut editor);
    assert_eq!(port.checks(), 1);
    assert_eq!(
        editor.status.text,
        format!(
            "Luxforge {} is up to date",
            luxforge_core::update::CURRENT_VERSION
        )
    );
    finish(editor, root);
}

/// Each control's call is the one an independent client sends for it: the same body answers the
/// same way, so the offer's buttons are no more than their commands.
#[test]
fn each_control_is_one_command() {
    let (port, mut editor, root) = sparkle();
    offers(&port, FakeUpdatePort::release("0.2.0", 2));
    reread(&mut editor);
    let agent = editor.owner.register();
    // Skip this version is `update.skip {version}`.
    assert_eq!(calls::skip_params("0.2.0"), json!({"version": "0.2.0"}));
    // Install and relaunch is `update.install` with a mutation, which an agent without
    // permission authority is refused and the desktop is not.
    let params = calls::install_params("parity-install");
    assert_eq!(params["mutation"]["request_id"], "parity-install");
    let refused = call_own(&editor.owner, agent, "update.install", params.clone());
    assert!(refused.is_err(), "an agent client may not install");
    let (answer, _) = call_own(&editor.owner, editor.client, "update.install", params).unwrap();
    assert_eq!(answer["outcome"], "requested");
    assert_eq!(port.installs(), 1);
    // Check now and Check for Updates are `update.check`, Later and Release notes send nothing.
    let checks = port.checks();
    let (_, _) = call(&editor.owner, agent, "update.check", json!({})).unwrap();
    assert_eq!(port.checks(), checks + 1);
    let installs = port.installs();
    press(&mut editor, NoticeButton::ReleaseNotes);
    assert_eq!(port.installs(), installs);
    assert!(notice(&editor.updates).is_some());
    finish(editor, root);
}
