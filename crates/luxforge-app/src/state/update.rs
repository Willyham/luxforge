//! Automatic updates as this desktop shows them: the offer in the workspace, the Updates group of
//! Settings › General and the words for each of the core's answers
//! ([design](../../../../docs/design/auto-update.md#the-desktop)).
//!
//! The update state is the host's. `update.status` says what the mechanism is, what the last check
//! found and whether an update is downloading or ready; `update.check`, `update.install` and
//! `update.skip` act on it, and the two toggles are preferences written through `preferences.set`.
//! What is held here is plain data: the status as last read, the calls in flight, the refusal the
//! last install met and Later, the one choice that is this window's own: it hides the offer until
//! the next check outcome, and writes nothing.
use luxforge_core::update::{
    AvailableUpdate, CheckOutcome, CurrentBuild, LastCheck, Mechanism, Progress, UnavailableReason,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::coalesce::Coalesce;

/// The one line the Updates group says about what a check sends.
pub(crate) const DISCLOSURE: &str =
    "Checks contact api.luxforge.app and send only the app version.";
/// The Updates group's two switches.
pub(crate) const CHECKS: &str = "Check for updates automatically";
pub(crate) const INSTALL_AUTOMATICALLY: &str = "Install updates automatically";
/// What the second switch says under its title.
pub(crate) const INSTALL_DESCRIPTION: &str = "Downloads in the background and installs when you \
     quit Luxforge. An update that needs a new catalog is always offered instead.";
/// Check now.
pub(crate) const CHECK_NOW: &str = "Check now";
/// What a manual check in flight shows.
pub(crate) const CHECKING: &str = "Checking for updates\u{2026}";
/// What the offer says of an update that changes the catalog format.
pub(crate) const NEW_CATALOG: &str = "This version needs a new catalog. Your current catalog stays \
     as it is, and the version you have now keeps opening it.";

/// What `update.status` answers, and the `status` an `update.skip` or `update.install` answer
/// carries. The reason is kept as the wire names it, so a reason this desktop has no words for
/// still reads as unavailable rather than failing the read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct UpdateRead {
    pub(crate) mechanism: Mechanism,
    #[serde(default)]
    pub(crate) reason: Option<String>,
    pub(crate) current: CurrentBuild,
    pub(crate) checks: bool,
    pub(crate) install_automatically: bool,
    #[serde(default)]
    pub(crate) checking: bool,
    #[serde(default)]
    pub(crate) last_check: Option<LastCheck>,
    #[serde(default)]
    pub(crate) available: Option<AvailableUpdate>,
    #[serde(default)]
    pub(crate) progress: Option<Progress>,
    #[serde(default)]
    pub(crate) ready: bool,
    #[serde(default)]
    pub(crate) install_requested: bool,
    #[serde(default)]
    pub(crate) error: Option<String>,
    #[serde(default)]
    pub(crate) skipped_version: Option<String>,
}

/// An owner answer as the status it reports: `update.status`'s and `update.check`'s whole answer,
/// or the `status` field of an `update.skip` or `update.install` answer.
pub(crate) fn parse(answer: Value) -> Result<UpdateRead, String> {
    serde_json::from_value(answer).map_err(|error| format!("unreadable update status: {error}"))
}

/// Why an installation does not update itself, in plain words, for each reason the core names,
/// and a general sentence for one this build has no words for.
pub(crate) fn reason_text(reason: Option<&str>) -> String {
    const WORDS: [(UnavailableReason, &str); 5] = [
        (
            UnavailableReason::DevelopmentBuild,
            "This development build does not update itself.",
        ),
        (
            UnavailableReason::Headless,
            "This launch has no updater, so it does not update itself.",
        ),
        (
            UnavailableReason::NoUpdateKey,
            "This build was made without the update signing key, so it cannot verify an update \
             and does not update itself.",
        ),
        (
            UnavailableReason::OutsideApplications,
            "Luxforge can only update itself from the Applications folder. Move it there to get \
             updates.",
        ),
        (
            UnavailableReason::ReadOnlyInstall,
            "Luxforge cannot replace its own files where it is installed, so it does not update \
             itself. Download new versions from the releases page.",
        ),
    ];
    reason
        .and_then(|reason| {
            WORDS
                .iter()
                .find(|(known, _)| known.as_str() == reason)
                .map(|(_, words)| (*words).to_owned())
        })
        .unwrap_or_else(|| match reason {
            Some(reason) => format!("Updates are unavailable for this installation ({reason})."),
            None => "Updates are unavailable for this installation.".to_owned(),
        })
}

/// How an `update.install` was refused, read from its code and data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// `conflict`: the running jobs `data.jobs` names, by kind.
    Conflict(Vec<String>),
    /// `not-ready`: its `data.reason`.
    NotReady(String),
    /// Anything else, as `code: message`.
    Other(String),
}

impl Refusal {
    /// The refusal an owner failure is, from its code and `data`.
    pub(crate) fn from_call(code: &str, message: &str, data: Option<&Value>) -> Self {
        match code {
            "conflict" => Self::Conflict(
                data.and_then(|data| data.get("jobs"))
                    .and_then(Value::as_array)
                    .map(|jobs| {
                        jobs.iter()
                            .filter_map(|job| job.get("kind").and_then(Value::as_str))
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            "not-ready" => Self::NotReady(
                data.and_then(|data| data.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
            ),
            _ => Self::Other(format!("{code}: {message}")),
        }
    }

    /// What the offer says of it.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Conflict(jobs) if jobs.is_empty() => {
                "Installing waits until the running work finishes.".to_owned()
            }
            Self::Conflict(jobs) => format!(
                "Installing waits until this work finishes: {}.",
                jobs.iter()
                    .map(|kind| job_words(kind))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::NotReady(reason) => match reason.as_str() {
                "notify-only" => {
                    "This installation cannot replace itself. Download the new version instead."
                        .to_owned()
                }
                "no-update" => "There is no update to install now.".to_owned(),
                other => reason_text(Some(other)),
            },
            Self::Other(text) => format!("Could not install the update: {text}"),
        }
    }
}

/// A running job an update waits for, by its kind as `data.jobs` names it.
fn job_words(kind: &str) -> String {
    match kind {
        "export" => "an export".into(),
        "batch-export" => "a batch export".into(),
        "hdr-merge" => "an HDR merge".into(),
        "panorama-merge" => "a panorama merge".into(),
        "develop-picks" => "developing picks".into(),
        "batch-settings" => "applying settings to photos".into(),
        "install" => "a module install".into(),
        "remove" => "a module removal".into(),
        "task" => "a module task".into(),
        other => other.replace('-', " "),
    }
}

/// One of the offer's buttons, in the order it draws them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeButton {
    /// Opens the release page through the checked HTTPS opener.
    ReleaseNotes,
    /// `update.install`.
    Install,
    /// A notify-only installation's Install: opens the release page.
    Download,
    /// Hides the offer until the next check outcome.
    Later,
    /// `update.skip`.
    Skip,
}

impl NoticeButton {
    pub(crate) const ALL: [Self; 5] = [
        Self::ReleaseNotes,
        Self::Install,
        Self::Download,
        Self::Later,
        Self::Skip,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::ReleaseNotes => "Release notes",
            Self::Install => "Install and relaunch",
            Self::Download => "Download",
            Self::Later => "Later",
            Self::Skip => "Skip this version",
        }
    }

    /// The name an evidence step and a frame's state use.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::ReleaseNotes => "release_notes",
            Self::Install => "install",
            Self::Download => "download",
            Self::Later => "later",
            Self::Skip => "skip",
        }
    }

    pub(crate) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|button| button.name() == name)
    }
}

/// This desktop's update state: the status as last read and what is in flight.
#[derive(Clone, Debug, Default)]
pub(crate) struct Updates {
    /// The status as the last `update.status`, `update.check`, `update.skip` or `update.install`
    /// answered it, or `None` before the first answer.
    pub(crate) status: Option<UpdateRead>,
    /// Why the last read failed, until one succeeds.
    pub(crate) read_error: Option<String>,
    /// One `update.status` read in flight, and one more wanted behind it.
    pub(crate) reading: Coalesce<()>,
    /// A check this window asked for (Check now or the palette) is in flight. Only such a check
    /// shows that it is checking; the port's own schedule checks quietly.
    pub(crate) checking: bool,
    /// Why the last check this window asked for was refused or failed, until the next one.
    pub(crate) check_error: Option<String>,
    /// Later was pressed after this check outcome: the offer stays hidden until another.
    pub(crate) later: Option<Option<LastCheck>>,
    /// An `update.install` is in flight.
    pub(crate) installing: bool,
    /// How the last `update.install` was refused, for the version it asked to install.
    pub(crate) refusal: Option<(String, Refusal)>,
    /// The version an `update.skip` in flight stops offering.
    pub(crate) skipping: Option<String>,
    /// Why the last skip or page opening failed, until the next one succeeds.
    pub(crate) failure: Option<String>,
    /// The page the last Release notes or Download opened, or recorded where no browser opens.
    pub(crate) opened: Option<String>,
    /// The offer as last recorded in the log: its title, lines, error and buttons.
    pub(crate) shown: Option<Shown>,
}

/// What the log records of the offer on screen.
pub(crate) type Shown = (String, Vec<String>, Option<String>, Vec<&'static str>);

impl Updates {
    /// The release on offer: available, not skipped and not being skipped, from a mechanism that
    /// is not unavailable.
    pub(crate) fn offered(&self) -> Option<&AvailableUpdate> {
        let status = self.status.as_ref()?;
        if status.mechanism == Mechanism::Unavailable {
            return None;
        }
        status
            .available
            .as_ref()
            .filter(|available| !available.skipped)
            .filter(|available| self.skipping.as_deref() != Some(available.version.as_str()))
    }

    /// Later hides the offer: no check has ended since it was pressed.
    pub(crate) fn later_hides(&self) -> bool {
        match (&self.later, &self.status) {
            (Some(at), Some(status)) => *at == status.last_check,
            _ => false,
        }
    }
}

/// The offer as the workspace draws it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct UpdateNotice {
    /// "Luxforge 0.2.0 is available".
    pub(crate) title: String,
    /// The sentences under the title, in order.
    pub(crate) lines: Vec<String>,
    /// The download's fraction while it runs: `Some(None)` while its length is unknown.
    pub(crate) progress: Option<Option<f32>>,
    /// The last install's refusal or a failure, drawn as an error.
    pub(crate) error: Option<String>,
    pub(crate) buttons: Vec<NoticeButton>,
    /// A call is in flight, so the buttons that send one wait.
    pub(crate) busy: bool,
    /// The release page, for Release notes and Download.
    pub(crate) url: Option<String>,
}

/// The offer for this state, while there is one to show.
pub(crate) fn notice(updates: &Updates) -> Option<UpdateNotice> {
    let available = updates.offered()?;
    if updates.later_hides() {
        return None;
    }
    let status = updates.status.as_ref()?;
    let notify = status.mechanism == Mechanism::LinuxNotify;
    let url = available
        .notes_url
        .as_deref()
        .and_then(|url| crate::browser::checked(url).ok())
        .map(str::to_owned);
    let mut lines = Vec::new();
    if available.catalog_format_change {
        lines.push(NEW_CATALOG.to_owned());
    }
    let mut progress = None;
    if status.install_requested {
        lines.push(format!(
            "Installing Luxforge {}. Luxforge quits and relaunches when it is ready.",
            available.version
        ));
    } else if notify {
        lines.push(
            "This installation cannot replace itself. Download the new version from its release \
             page."
                .to_owned(),
        );
    } else if status.ready {
        lines.push("Downloaded and ready to install.".to_owned());
    } else if let Some(download) = status.progress {
        let fraction = download
            .total
            .filter(|total| *total > 0)
            .map(|total| (download.received as f64 / total as f64).clamp(0.0, 1.0) as f32);
        lines.push(match fraction {
            Some(fraction) => format!("Downloading\u{2026} {}%", (fraction * 100.0).floor() as u32),
            None => format!("Downloading\u{2026} {}", bytes_text(download.received)),
        });
        progress = Some(fraction);
    }
    if available.installs_automatically && !status.install_requested && !notify {
        lines.push("It installs when you next quit Luxforge.".to_owned());
    }
    if updates.checking {
        lines.push(CHECKING.to_owned());
    }
    let error = updates
        .refusal
        .as_ref()
        .filter(|(version, _)| *version == available.version)
        .map(|(_, refusal)| refusal.text())
        .or_else(|| updates.failure.clone());
    let mut buttons = Vec::new();
    if url.is_some() && !notify {
        buttons.push(NoticeButton::ReleaseNotes);
    }
    if !status.install_requested {
        if notify {
            if url.is_some() {
                buttons.push(NoticeButton::Download);
            }
        } else {
            buttons.push(NoticeButton::Install);
        }
        buttons.push(NoticeButton::Later);
        buttons.push(NoticeButton::Skip);
    }
    Some(UpdateNotice {
        title: format!("Luxforge {} is available", available.version),
        lines,
        progress,
        error,
        buttons,
        busy: updates.installing || updates.skipping.is_some(),
        url,
    })
}

/// A byte count as the offer says it.
fn bytes_text(bytes: u64) -> String {
    const MB: f64 = 1_000_000.0;
    if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1000))
    }
}

/// The Updates group of Settings › General as the sheet draws it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct UpdatesGroup {
    /// "Luxforge 0.1.0 (build 41)".
    pub(crate) version: String,
    /// Why this installation does not update itself, shown in place of the controls.
    pub(crate) unavailable: Option<String>,
    /// The status has not been read yet.
    pub(crate) loading: bool,
    pub(crate) checks: bool,
    pub(crate) install_automatically: bool,
    /// Install updates automatically takes a change: checks are on.
    pub(crate) install_enabled: bool,
    /// A change to either switch has not been answered yet.
    pub(crate) checks_saving: bool,
    pub(crate) install_saving: bool,
    /// A check this window asked for is in flight.
    pub(crate) checking: bool,
    /// The last check's time and outcome.
    pub(crate) last_check: String,
    /// A failed read, check or last check's error.
    pub(crate) error: Option<String>,
}

/// The preferences the group's switches show: the stored answer with the changes outstanding laid
/// over it, as every General row shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Switches {
    pub(crate) checks: bool,
    pub(crate) install_automatically: bool,
    pub(crate) checks_saving: bool,
    pub(crate) install_saving: bool,
}

/// The Updates group for this state, the switches the preference writer shows and the time now.
pub(crate) fn group(updates: &Updates, switches: Option<Switches>, now_ms: i64) -> UpdatesGroup {
    let version = match &updates.status {
        Some(status) => version_text(&status.current),
        None => format!("Luxforge {}", luxforge_core::update::CURRENT_VERSION),
    };
    let Some(status) = &updates.status else {
        return UpdatesGroup {
            version,
            loading: updates.read_error.is_none(),
            error: updates
                .read_error
                .as_ref()
                .map(|reason| format!("Could not read the update status: {reason}")),
            ..UpdatesGroup::default()
        };
    };
    if status.mechanism == Mechanism::Unavailable {
        return UpdatesGroup {
            version,
            unavailable: Some(reason_text(status.reason.as_deref())),
            ..UpdatesGroup::default()
        };
    }
    let switches = switches.unwrap_or(Switches {
        checks: status.checks,
        install_automatically: status.install_automatically,
        checks_saving: false,
        install_saving: false,
    });
    let error = updates
        .check_error
        .clone()
        .or_else(|| {
            updates
                .read_error
                .as_ref()
                .map(|reason| format!("Could not read the update status: {reason}"))
        })
        .or_else(|| {
            status
                .error
                .as_ref()
                .filter(|_| {
                    status
                        .last_check
                        .is_some_and(|check| check.outcome == CheckOutcome::Failed)
                })
                .cloned()
        });
    UpdatesGroup {
        version,
        unavailable: None,
        loading: false,
        checks: switches.checks,
        install_automatically: switches.install_automatically,
        install_enabled: switches.checks,
        checks_saving: switches.checks_saving,
        install_saving: switches.install_saving,
        checking: updates.checking,
        last_check: last_check_text(status, now_ms),
        error,
    }
}

/// "Luxforge 0.1.0", with the build when the port reports one.
pub(crate) fn version_text(current: &CurrentBuild) -> String {
    match current.build {
        Some(build) => format!("Luxforge {} (build {build})", current.version),
        None => format!("Luxforge {}", current.version),
    }
}

/// When the last check ended and what it found, as the group says it.
pub(crate) fn last_check_text(status: &UpdateRead, now_ms: i64) -> String {
    let Some(check) = status.last_check else {
        return "Not checked yet".to_owned();
    };
    let when = ago(now_ms.saturating_sub(check.at_ms));
    let outcome = match check.outcome {
        CheckOutcome::UpToDate => "Luxforge is up to date".to_owned(),
        CheckOutcome::Found => match &status.available {
            Some(available) => format!("found Luxforge {}", available.version),
            None => "found an update".to_owned(),
        },
        CheckOutcome::Failed => "the check failed".to_owned(),
    };
    format!("Last checked {when}: {outcome}")
}

/// How long ago, in whole units.
fn ago(ms: i64) -> String {
    let minutes = ms / 60_000;
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{n} {unit}s ago")
        }
    };
    match minutes {
        ..1 => "just now".to_owned(),
        1..60 => plural(minutes, "minute"),
        60..1440 => plural(minutes / 60, "hour"),
        _ => plural(minutes / 1440, "day"),
    }
}

/// What the status bar says once a check this window asked for has answered.
pub(crate) fn check_outcome_text(status: &UpdateRead) -> String {
    if status.checking {
        return CHECKING.to_owned();
    }
    match status.last_check.map(|check| check.outcome) {
        Some(CheckOutcome::Found) => match &status.available {
            Some(available) if available.skipped => format!(
                "Luxforge {} is available; you skipped this version",
                available.version
            ),
            Some(available) => format!("Luxforge {} is available", available.version),
            None => format!("Luxforge {} is up to date", status.current.version),
        },
        Some(CheckOutcome::Failed) => format!(
            "Could not check for updates: {}",
            status.error.as_deref().unwrap_or("the check failed")
        ),
        Some(CheckOutcome::UpToDate) | None => {
            format!("Luxforge {} is up to date", status.current.version)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use luxforge_core::update::CATALOG_FORMAT;
    use serde_json::json;

    fn status(mechanism: &str) -> UpdateRead {
        parse(json!({
            "mechanism": mechanism, "reason": null,
            "current": {"version": "0.1.0", "build": 41},
            "checks": true, "install_automatically": false, "prerelease_channel": false,
            "checking": false, "last_check": {"at_ms": 1_000, "outcome": "found"},
            "available": {"version": "0.2.0", "build": 42,
                          "notes_url": "https://github.com/luxforge/luxforge/releases/tag/v0.2.0",
                          "catalog_format": CATALOG_FORMAT, "catalog_format_change": false,
                          "prerelease": false, "skipped": false, "installs_automatically": false},
            "progress": null, "ready": false, "install_requested": false, "error": null,
            "skipped_version": null, "can_move_to_applications": false
        }))
        .unwrap()
    }

    fn offered(status: UpdateRead) -> Updates {
        Updates {
            status: Some(status),
            ..Updates::default()
        }
    }

    #[test]
    fn update_every_reason_has_its_own_words_and_an_unknown_one_a_general_sentence() {
        let reasons = [
            UnavailableReason::DevelopmentBuild,
            UnavailableReason::Headless,
            UnavailableReason::NoUpdateKey,
            UnavailableReason::OutsideApplications,
            UnavailableReason::ReadOnlyInstall,
        ];
        let fallback = reason_text(Some("a-reason-from-later"));
        assert_eq!(
            fallback,
            "Updates are unavailable for this installation (a-reason-from-later)."
        );
        let mut seen = std::collections::BTreeSet::new();
        for reason in reasons {
            let words = reason_text(Some(reason.as_str()));
            assert!(!words.starts_with("Updates are unavailable"), "{reason:?}");
            assert!(seen.insert(words), "{reason:?} has words of its own");
        }
        assert_eq!(
            reason_text(Some("development-build")),
            "This development build does not update itself."
        );
    }

    #[test]
    fn update_the_offer_names_the_version_and_offers_its_four_buttons() {
        let notice = notice(&offered(status("sparkle"))).unwrap();
        assert_eq!(notice.title, "Luxforge 0.2.0 is available");
        assert_eq!(
            notice.buttons,
            [
                NoticeButton::ReleaseNotes,
                NoticeButton::Install,
                NoticeButton::Later,
                NoticeButton::Skip
            ]
        );
        assert!(notice.lines.is_empty() && notice.error.is_none());
    }

    #[test]
    fn update_a_catalog_format_change_says_the_current_catalog_stays_usable() {
        let mut read = status("sparkle");
        read.available.as_mut().unwrap().catalog_format_change = true;
        let notice = notice(&offered(read)).unwrap();
        assert_eq!(notice.lines, [NEW_CATALOG]);
        assert!(NEW_CATALOG.contains("needs a new catalog"));
        assert!(NEW_CATALOG.contains("keeps opening it"));
    }

    #[test]
    fn update_progress_and_ready_are_said() {
        let mut read = status("linux-payload");
        read.progress = Some(Progress {
            received: 4_500_000,
            total: Some(10_000_000),
        });
        let downloading = notice(&offered(read.clone())).unwrap();
        assert_eq!(downloading.lines, ["Downloading\u{2026} 45%"]);
        assert_eq!(downloading.progress, Some(Some(0.45)));
        read.progress = None;
        read.ready = true;
        let ready = notice(&offered(read)).unwrap();
        assert_eq!(ready.lines, ["Downloaded and ready to install."]);
        assert_eq!(ready.progress, None);
    }

    #[test]
    fn update_a_notify_only_installation_downloads_instead() {
        let notice = notice(&offered(status("linux-notify"))).unwrap();
        assert_eq!(
            notice.buttons,
            [
                NoticeButton::Download,
                NoticeButton::Later,
                NoticeButton::Skip
            ]
        );
        assert!(notice.url.unwrap().starts_with("https://"));
    }

    #[test]
    fn update_later_hides_the_offer_until_the_next_check_outcome() {
        let mut updates = offered(status("sparkle"));
        updates.later = Some(updates.status.as_ref().unwrap().last_check);
        assert!(notice(&updates).is_none());
        updates.status.as_mut().unwrap().last_check = Some(LastCheck {
            at_ms: 2_000,
            outcome: CheckOutcome::Found,
        });
        assert!(notice(&updates).is_some(), "a later check offers it again");
    }

    #[test]
    fn update_skipped_unavailable_and_skipping_offer_nothing() {
        let mut skipped = status("sparkle");
        skipped.available.as_mut().unwrap().skipped = true;
        assert!(notice(&offered(skipped)).is_none());
        let mut unavailable = status("unavailable");
        unavailable.reason = Some("development-build".into());
        assert!(notice(&offered(unavailable)).is_none());
        let mut skipping = offered(status("sparkle"));
        skipping.skipping = Some("0.2.0".into());
        assert!(notice(&skipping).is_none(), "a skip shows at once");
    }

    #[test]
    fn update_a_refused_install_names_the_running_jobs_or_the_reason() {
        let conflict = Refusal::from_call(
            "conflict",
            "installing an update waits until no job runs: export j1",
            Some(&json!({"jobs": [{"job_id": "j1", "kind": "export"},
                                  {"job_id": "j2", "kind": "hdr-merge"}]})),
        );
        assert_eq!(
            conflict,
            Refusal::Conflict(vec!["export".into(), "hdr-merge".into()])
        );
        assert_eq!(
            conflict.text(),
            "Installing waits until this work finishes: an export, an HDR merge."
        );
        let mut updates = offered(status("sparkle"));
        updates.refusal = Some(("0.2.0".into(), conflict));
        assert!(
            notice(&updates)
                .unwrap()
                .error
                .unwrap()
                .contains("an HDR merge")
        );
        let not_ready =
            Refusal::from_call("not-ready", "x", Some(&json!({"reason": "notify-only"})));
        assert_eq!(not_ready, Refusal::NotReady("notify-only".into()));
        assert!(not_ready.text().contains("Download"));
        assert_eq!(
            Refusal::from_call("not-ready", "x", Some(&json!({"reason": "no-update-key"}))).text(),
            reason_text(Some("no-update-key"))
        );
    }

    #[test]
    fn update_the_group_shows_the_reason_instead_of_the_controls() {
        let mut read = status("unavailable");
        read.reason = Some("development-build".into());
        read.available = None;
        let group = group(&offered(read), None, 0);
        assert_eq!(group.version, "Luxforge 0.1.0 (build 41)");
        assert_eq!(
            group.unavailable.as_deref(),
            Some("This development build does not update itself.")
        );
    }

    #[test]
    fn update_the_group_says_when_it_last_checked_and_what_it_found() {
        let updates = offered(status("sparkle"));
        let switches = Switches {
            checks: false,
            install_automatically: true,
            checks_saving: true,
            install_saving: false,
        };
        let group = group(&updates, Some(switches), 1_000 + 3 * 60_000);
        assert_eq!(
            group.last_check,
            "Last checked 3 minutes ago: found Luxforge 0.2.0"
        );
        assert!(!group.checks && group.install_automatically && !group.install_enabled);
        assert!(group.checks_saving);
        let mut never = status("sparkle");
        never.last_check = None;
        assert_eq!(last_check_text(&never, 0), "Not checked yet");
        assert_eq!(ago(30_000), "just now");
        assert_eq!(ago(2 * 3_600_000), "2 hours ago");
        assert_eq!(ago(86_400_000), "1 day ago");
    }
}
