//! Automatic updates, as the core owns them (`docs/design/auto-update.md`): the update state
//! `update.status` answers, the preferences and policy that decide what may happen without the
//! person, and the host port a platform's mechanism implements.
//!
//! The core owns the state, the commands and the preferences; it never touches the network, a feed
//! or an archive. A host that can update itself implements [`UpdatePort`] (the desktop: Sparkle on
//! macOS, the payload updater on Linux) and passes it in [`crate::HostConfig::updates`]. A host
//! with no port, such as `luxforge-json`, reports [`Mechanism::Unavailable`] with
//! [`UnavailableReason::Headless`]; a desktop that may not update itself, such as a development
//! build, passes an [`UnavailablePort`] naming its reason, so nothing is ever checked or installed.
//!
//! Two rules are encoded here rather than left to each port:
//!
//! - **A catalog format change is visible and never automatic.** Each feed item carries its
//!   release's catalog format; the core compares it with [`CATALOG_FORMAT`], so every
//!   [`AvailableUpdate`] says whether it changes the format, and
//!   [`UpdatePolicy::installs_automatically`] is false for such an update whatever the person's
//!   preference.
//! - **Every channel is followed.** A client follows the default and `prerelease` channels, so a
//!   release candidate is offered as a release is.
use crate::Error;
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

#[cfg(any(test, feature = "test-holds"))]
pub mod testing;

/// The catalog format this build reads and writes, which a feed item's `catalog_format` is compared
/// with.
pub const CATALOG_FORMAT: i64 = crate::editor::CATALOG_FORMAT;

/// This build's version, [`crate::APPLICATION_VERSION`].
pub const CURRENT_VERSION: &str = crate::APPLICATION_VERSION;

/// The longest a version string may be, in `update.skip` and in what a port reports.
pub const MAX_VERSION_LEN: usize = 64;

/// How this installation updates itself, as `update.status` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mechanism {
    /// Sparkle 2, on macOS.
    Sparkle,
    /// The in-house updater replacing a writable Linux `Luxforge/` payload.
    LinuxPayload,
    /// A Linux installation that cannot replace itself: an update is offered as its release page.
    LinuxNotify,
    /// No updater: [`UpdateStatus::reason`] says why.
    Unavailable,
}

/// Why an installation has no updater. More reasons may be added; a client shows an unknown one
/// as unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum UnavailableReason {
    /// A development or test build, which never contacts the feed.
    DevelopmentBuild,
    /// A host without a desktop, such as `luxforge-json`: no port at all.
    Headless,
    /// A release build without the public update key.
    NoUpdateKey,
    /// A macOS app outside `/Applications` and `~/Applications`, translocated or on a read-only
    /// volume, which cannot replace itself until it moves (`update.move-to-applications`).
    OutsideApplications,
    /// An installation whose files cannot be replaced, where no notify-only offer applies.
    ReadOnlyInstall,
}

impl UnavailableReason {
    /// The reason's wire name, as `update.status` and a `not-ready` refusal's `data.reason` give it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DevelopmentBuild => "development-build",
            Self::Headless => "headless",
            Self::NoUpdateKey => "no-update-key",
            Self::OutsideApplications => "outside-applications",
            Self::ReadOnlyInstall => "read-only-install",
        }
    }
}

/// What a port says its mechanism is: one that works, or none and why. The core splits it into
/// [`UpdateStatus::mechanism`] and [`UpdateStatus::reason`], so an unavailable port always names a
/// reason and a working one never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Updater {
    Sparkle,
    LinuxPayload,
    LinuxNotify,
    Unavailable(UnavailableReason),
}

impl Updater {
    pub fn mechanism(self) -> Mechanism {
        match self {
            Self::Sparkle => Mechanism::Sparkle,
            Self::LinuxPayload => Mechanism::LinuxPayload,
            Self::LinuxNotify => Mechanism::LinuxNotify,
            Self::Unavailable(_) => Mechanism::Unavailable,
        }
    }

    pub fn reason(self) -> Option<UnavailableReason> {
        match self {
            Self::Unavailable(reason) => Some(reason),
            _ => None,
        }
    }
}

/// One feed item a port found newer than this build, on a channel it read: the platform half of
/// [`AvailableUpdate`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FeedUpdate {
    /// `sparkle:shortVersionString`, such as `0.2.0`.
    pub version: String,
    /// `sparkle:version`, the build number that orders releases.
    pub build: u64,
    /// The release page (the item's link), opened by Release notes.
    pub notes_url: Option<String>,
    /// `luxforge:catalogFormat`: the catalog format the release reads and writes.
    pub catalog_format: i64,
    /// The item is on the `prerelease` channel.
    pub prerelease: bool,
}

/// The outcome of the last finished check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckOutcome {
    /// The feed had nothing newer on a followed channel.
    UpToDate,
    /// The feed had a newer release: [`UpdateStatus::available`].
    Found,
    /// The check could not finish: [`UpdateStatus::error`] says why.
    Failed,
}

/// When the last check finished and what it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastCheck {
    /// Milliseconds since the Unix epoch.
    pub at_ms: i64,
    pub outcome: CheckOutcome,
}

/// How far the available update's download has come.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// Bytes received so far.
    pub received: u64,
    /// The archive's stated length, when known.
    pub total: Option<u64>,
}

/// What a port reports of its state ([`UpdatePort::status`]). The core adds the version, the
/// preferences, the catalog format comparison and the skip, and answers [`UpdateStatus`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortStatus {
    pub updater: Updater,
    /// This installation's build number (`CFBundleVersion`, or `build.json`'s), when it has one.
    pub build: Option<u64>,
    /// A check is running now, started by `update.check` or by the port's own schedule.
    pub checking: bool,
    pub last_check: Option<LastCheck>,
    /// The newest release the last successful check found, kept until it is installed or a later
    /// check finds none.
    pub available: Option<FeedUpdate>,
    /// The available update's download, while it runs.
    pub progress: Option<Progress>,
    /// The available update is downloaded and verified, and installs at the end of the quit path.
    pub ready: bool,
    /// The last failure, in words: a check, a download, a verification or an install.
    pub error: Option<String>,
    /// `update.move-to-applications` can move this app (macOS, outside Applications).
    pub can_move_to_applications: bool,
}

impl PortStatus {
    /// A port with nothing to report: `updater`, and nothing checked yet.
    pub fn idle(updater: Updater) -> Self {
        Self {
            updater,
            build: None,
            checking: false,
            last_check: None,
            available: None,
            progress: None,
            ready: false,
            error: None,
            can_move_to_applications: false,
        }
    }
}

/// This installation's version and build.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentBuild {
    /// [`CURRENT_VERSION`].
    pub version: String,
    /// The build number the port reports; `null` without a port.
    pub build: Option<u64>,
}

/// A release newer than this build, as `update.status` offers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableUpdate {
    pub version: String,
    pub build: u64,
    pub notes_url: Option<String>,
    /// The release's catalog format.
    pub catalog_format: i64,
    /// The release reads and writes another catalog format than this build
    /// ([`CATALOG_FORMAT`]): it needs a new catalog, and is never installed automatically.
    pub catalog_format_change: bool,
    pub prerelease: bool,
    /// The person skipped this version (`update.skip`), so it is not offered.
    pub skipped: bool,
    /// It downloads in the background and installs at the next ordinary quit without asking:
    /// [`UpdatePolicy::installs_automatically`].
    pub installs_automatically: bool,
}

/// What `update.status` answers, and what `update.check`, `update.skip` and `update.install` answer
/// as `status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub mechanism: Mechanism,
    /// Why there is no updater, exactly when `mechanism` is `unavailable`.
    pub reason: Option<UnavailableReason>,
    pub current: CurrentBuild,
    /// The `update_checks` preference: check at launch and daily.
    pub checks: bool,
    /// The `update_install_automatically` preference as stored. It applies only while `checks` is
    /// on, and never to an update that changes the catalog format.
    pub install_automatically: bool,
    pub checking: bool,
    pub last_check: Option<LastCheck>,
    pub available: Option<AvailableUpdate>,
    pub progress: Option<Progress>,
    /// The available update is downloaded and verified.
    pub ready: bool,
    /// `update.install` was accepted: the desktop is running its quit path.
    pub install_requested: bool,
    pub error: Option<String>,
    /// The version `update.skip` stored, if any.
    pub skipped_version: Option<String>,
    pub can_move_to_applications: bool,
}

impl UpdateStatus {
    /// The status from what the port reports (none for a host without one), the policy and whether
    /// an install was requested.
    pub fn compose(
        port: Option<&PortStatus>,
        policy: &UpdatePolicy,
        install_requested: bool,
    ) -> Self {
        let headless = PortStatus::idle(Updater::Unavailable(UnavailableReason::Headless));
        let port = port.unwrap_or(&headless);
        let unavailable = port.updater.mechanism() == Mechanism::Unavailable;
        let available = port
            .available
            .as_ref()
            .filter(|_| !unavailable)
            .map(|update| AvailableUpdate {
                version: update.version.clone(),
                build: update.build,
                notes_url: update.notes_url.clone(),
                catalog_format: update.catalog_format,
                catalog_format_change: policy.changes_catalog_format(update),
                prerelease: update.prerelease,
                skipped: policy.skipped(update),
                installs_automatically: policy.installs_automatically(update),
            });
        Self {
            mechanism: port.updater.mechanism(),
            reason: port.updater.reason(),
            current: CurrentBuild {
                version: CURRENT_VERSION.to_owned(),
                build: port.build,
            },
            checks: policy.checks,
            install_automatically: policy.install_automatically,
            checking: port.checking && !unavailable,
            last_check: port.last_check,
            progress: port.progress.filter(|_| available.is_some()),
            ready: port.ready && available.is_some(),
            available,
            install_requested,
            error: port.error.clone(),
            skipped_version: policy.skipped_version.clone(),
            can_move_to_applications: port.can_move_to_applications,
        }
    }

    /// The same status without its download progress: what must change for the owner to announce
    /// it, so a running download's progress alone does not fill the event log.
    pub(crate) fn settled(&self) -> Self {
        Self {
            progress: None,
            ..self.clone()
        }
    }
}

/// The person's update choices as a port applies them, and the rules derived from them that no
/// port may decide differently. The owner hands a port one at its start and after every change
/// ([`UpdatePort::apply`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpdatePolicy {
    /// `update_checks`: check at launch and then daily.
    pub checks: bool,
    /// `update_install_automatically`, as stored.
    pub install_automatically: bool,
    /// The version `update.skip` stored.
    pub skipped_version: Option<String>,
    catalog_format: i64,
}

impl UpdatePolicy {
    pub fn new(checks: bool, install_automatically: bool, skipped_version: Option<String>) -> Self {
        Self {
            checks,
            install_automatically,
            skipped_version,
            catalog_format: CATALOG_FORMAT,
        }
    }

    /// The catalog format this build reads and writes ([`CATALOG_FORMAT`]).
    pub fn catalog_format(&self) -> i64 {
        self.catalog_format
    }

    /// Whether `update` reads and writes another catalog format than this build.
    pub fn changes_catalog_format(&self, update: &FeedUpdate) -> bool {
        update.catalog_format != self.catalog_format
    }

    /// Whether the person skipped `update`'s version, so it is not offered.
    pub fn skipped(&self, update: &FeedUpdate) -> bool {
        self.skipped_version.as_deref() == Some(update.version.as_str())
    }

    /// Whether `update` may download in the background and install at the next ordinary quit
    /// without asking: only while checks and automatic installation are both on, for a version not
    /// skipped, and never when it changes the catalog format. A port
    /// installs nothing automatically unless this says so.
    pub fn installs_automatically(&self, update: &FeedUpdate) -> bool {
        self.checks
            && self.install_automatically
            && !self.skipped(update)
            && !self.changes_catalog_format(update)
    }
}

/// Shared between the owner and the notifiers it hands out.
struct Signal {
    /// The port changed something since the owner last looked.
    dirty: AtomicBool,
    /// The highest check generation a [`CheckDone`] has finished.
    finished: AtomicU64,
    /// Wakes the owner; must never block.
    wake: Box<dyn Fn() + Send + Sync>,
}

/// How a port tells the catalog owner its state changed. Calling it never blocks, from any thread,
/// the UI thread included: it sets a flag, and posts a wake only when the flag was clear, so at
/// most one wake waits for the owner however often it is called. The owner reads
/// [`UpdatePort::status`] once it wakes and announces an event when anything but download progress
/// moved, so a port may call it on every progress step.
#[derive(Clone)]
pub struct UpdateNotifier(Arc<Signal>);

impl UpdateNotifier {
    /// A notifier that calls `wake` each time it is signalled. The owner makes its own; a test of a
    /// port alone makes one to see what the port reports.
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self(Arc::new(Signal {
            dirty: AtomicBool::new(false),
            finished: AtomicU64::new(0),
            wake: Box::new(wake),
        }))
    }

    /// The port's state changed.
    pub fn changed(&self) {
        if !self.0.dirty.swap(true, Ordering::AcqRel) {
            (self.0.wake)();
        }
    }

    /// Take the signal: whether anything changed since the last take.
    pub(crate) fn take(&self) -> bool {
        self.0.dirty.swap(false, Ordering::AcqRel)
    }

    /// Whether the check of `generation` has finished.
    pub fn finished(&self, generation: u64) -> bool {
        self.0.finished.load(Ordering::Acquire) >= generation
    }

    fn finish(&self, generation: u64) {
        self.0.finished.fetch_max(generation, Ordering::AcqRel);
        self.changed();
    }
}

impl std::fmt::Debug for UpdateNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateNotifier")
            .field("finished", &self.0.finished.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

/// The completion of one check `update.check` started ([`UpdatePort::check`]). The port finishes it
/// once the check has ended, after its status reports the outcome; the calls waiting on the check
/// are then answered with that status. Dropping it finishes it too, so a check a port abandons
/// never holds a caller past the owner's own deadline.
#[must_use = "finish the check once its outcome is in the port's status"]
pub struct CheckDone {
    notifier: UpdateNotifier,
    generation: u64,
    done: bool,
}

impl CheckDone {
    pub(crate) fn new(notifier: UpdateNotifier, generation: u64) -> Self {
        Self {
            notifier,
            generation,
            done: false,
        }
    }

    /// Which check this is: each `update.check` that starts one takes the next number.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The check has ended and the port's status reports its outcome.
    pub fn finish(mut self) {
        self.complete();
    }

    fn complete(&mut self) {
        if !std::mem::replace(&mut self.done, true) {
            self.notifier.finish(self.generation);
        }
    }
}

impl Drop for CheckDone {
    fn drop(&mut self) {
        self.complete();
    }
}

impl std::fmt::Debug for CheckDone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckDone")
            .field("generation", &self.generation)
            .field("done", &self.done)
            .finish()
    }
}

/// The platform's update mechanism, which the desktop implements (Sparkle on macOS, the payload
/// updater on Linux) and passes in [`crate::HostConfig::updates`]. The catalog owner calls every
/// method on its own thread, so each returns at once: none touches the network or the disk, waits
/// on a download or holds a lock across either. The work runs on the port's own bounded workers,
/// and the port reports through its [`UpdateNotifier`].
///
/// The core validates every command before it reaches the port: authority, an available update,
/// running jobs and the mechanism's reason. A port only carries the command out.
pub trait UpdatePort: Send + Sync {
    /// The owner's notifier, handed over once each time an owner starts, before the first
    /// [`Self::apply`]. A port keeps the latest and signals it whenever its status changes.
    fn attach(&self, notifier: UpdateNotifier);

    /// The port's state now, from memory.
    fn status(&self) -> PortStatus;

    /// The person's choices and the rules derived from them, at the owner's start and after every
    /// change to `update_checks`, `update_install_automatically` or the skipped version. The port
    /// schedules checks only while [`UpdatePolicy::checks`] is on, follows the default and
    /// `prerelease` channels, and downloads and installs at the next ordinary quit only an update
    /// [`UpdatePolicy::installs_automatically`] allows.
    fn apply(&self, policy: &UpdatePolicy);

    /// Check the feed now, on the port's worker, and finish `done` once the outcome is in
    /// [`Self::status`]. A check already running may take `done` with it rather than start
    /// another. Only called while the mechanism is not unavailable.
    fn check(&self, done: CheckDone);

    /// Install the available update and relaunch. The desktop runs its ordinary quit path: drafts
    /// follow their usual rules, and only after the catalog owner has closed the catalog does the
    /// platform mechanism download what it still needs, replace the app and start the new version.
    /// The core has already refused the request unless an update is available, the mechanism can
    /// install and no job that must not be interrupted is running. An error is the port's own
    /// refusal, answered to the caller as it is.
    fn request_install(&self) -> Result<(), Error>;

    /// The person skipped `version`. The core has stored it, and the next [`Self::apply`] carries
    /// it; a port holding an offer for that version (Sparkle's reply block) answers it here.
    fn skip(&self, version: &str);

    /// Move the app into Applications and relaunch it from there, through the same quit path as
    /// [`Self::request_install`]. Only called when [`PortStatus::can_move_to_applications`].
    /// `replace` is the person's confirmation that a Luxforge already in the destination may go
    /// to the Trash; without it, a port refuses a move that would replace one with a `conflict`
    /// whose `data.reason` is `existing-app` and whose `data.path` names it.
    fn move_to_applications(&self, replace: bool) -> Result<(), Error>;
}

/// A port for a desktop that must not update itself: [`Updater::Unavailable`] with its reason, so
/// `update.status` names it, and nothing is ever checked or installed. A development build passes
/// one with [`UnavailableReason::DevelopmentBuild`]; a release build without the update key one with
/// [`UnavailableReason::NoUpdateKey`].
#[derive(Clone, Copy, Debug)]
pub struct UnavailablePort {
    pub reason: UnavailableReason,
    /// This installation's build number, when it has one.
    pub build: Option<u64>,
}

impl UnavailablePort {
    pub fn new(reason: UnavailableReason) -> Self {
        Self {
            reason,
            build: None,
        }
    }
}

impl UpdatePort for UnavailablePort {
    fn attach(&self, _: UpdateNotifier) {}

    fn status(&self) -> PortStatus {
        PortStatus {
            build: self.build,
            ..PortStatus::idle(Updater::Unavailable(self.reason))
        }
    }

    fn apply(&self, _: &UpdatePolicy) {}

    fn check(&self, done: CheckDone) {
        done.finish();
    }

    fn request_install(&self) -> Result<(), Error> {
        Err(Error::not_ready(format!(
            "this installation cannot update itself: {}",
            self.reason.as_str()
        )))
    }

    fn skip(&self, _: &str) {}

    fn move_to_applications(&self, _: bool) -> Result<(), Error> {
        Err(Error::not_ready(
            "this installation cannot move into Applications",
        ))
    }
}

/// Check a version string `update.skip` stores: 1 to [`MAX_VERSION_LEN`] ASCII letters, digits,
/// `.`, `-` and `+`.
pub(crate) fn check_version(version: &str) -> Result<(), String> {
    if version.is_empty()
        || version.len() > MAX_VERSION_LEN
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+'))
    {
        return Err(format!(
            "version must be 1..={MAX_VERSION_LEN} ASCII letters, digits, '.', '-' or '+'"
        ));
    }
    Ok(())
}

/// The policy the stored preferences give.
pub(crate) fn policy(preferences: &crate::preferences::Preferences) -> UpdatePolicy {
    UpdatePolicy::new(
        preferences.update_checks(),
        preferences.update_install_automatically(),
        preferences.update_skipped_version.clone(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(version: &str, catalog_format: i64, prerelease: bool) -> FeedUpdate {
        FeedUpdate {
            version: version.into(),
            build: 42,
            notes_url: Some("https://github.com/luxforge/luxforge/releases/tag/v0.2.0".into()),
            catalog_format,
            prerelease,
        }
    }

    #[test]
    fn update_a_catalog_format_change_is_never_installed_automatically() {
        let policy = UpdatePolicy::new(true, true, None);
        assert!(policy.installs_automatically(&item("0.2.0", CATALOG_FORMAT, false)));
        let changed = item("0.2.0", CATALOG_FORMAT + 1, false);
        assert!(policy.changes_catalog_format(&changed));
        assert!(!policy.installs_automatically(&changed));
        // Off, or with checks off, nothing installs automatically.
        for policy in [
            UpdatePolicy::new(true, false, None),
            UpdatePolicy::new(false, true, None),
            UpdatePolicy::new(true, true, Some("0.2.0".into())),
        ] {
            assert!(!policy.installs_automatically(&item("0.2.0", CATALOG_FORMAT, false)));
        }
        // A skip names one version: a later one installs as usual.
        let skipped = UpdatePolicy::new(true, true, Some("0.2.0".into()));
        assert!(skipped.installs_automatically(&item("0.2.1", CATALOG_FORMAT, false)));
    }

    #[test]
    fn update_status_composes_the_port_the_policy_and_the_catalog_format() {
        let policy = UpdatePolicy::new(true, true, None);
        let mut port = PortStatus::idle(Updater::Sparkle);
        port.build = Some(7);
        port.available = Some(item("0.2.0", CATALOG_FORMAT + 1, false));
        port.progress = Some(Progress {
            received: 10,
            total: Some(100),
        });
        let status = UpdateStatus::compose(Some(&port), &policy, false);
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({
                "mechanism": "sparkle",
                "reason": null,
                "current": {"version": CURRENT_VERSION, "build": 7},
                "checks": true,
                "install_automatically": true,
                "checking": false,
                "last_check": null,
                "available": {
                    "version": "0.2.0",
                    "build": 42,
                    "notes_url": "https://github.com/luxforge/luxforge/releases/tag/v0.2.0",
                    "catalog_format": CATALOG_FORMAT + 1,
                    "catalog_format_change": true,
                    "prerelease": false,
                    "skipped": false,
                    "installs_automatically": false,
                },
                "progress": {"received": 10, "total": 100},
                "ready": false,
                "install_requested": false,
                "error": null,
                "skipped_version": null,
                "can_move_to_applications": false,
            })
        );
        assert_eq!(
            serde_json::from_value::<UpdateStatus>(serde_json::to_value(&status).unwrap()).unwrap(),
            status
        );
        assert_eq!(status.settled().progress, None);
    }

    #[test]
    fn update_no_port_is_headless_and_a_prerelease_is_offered() {
        let policy = UpdatePolicy::new(true, false, None);
        let status = UpdateStatus::compose(None, &policy, false);
        assert_eq!(
            (status.mechanism, status.reason, status.current.build),
            (
                Mechanism::Unavailable,
                Some(UnavailableReason::Headless),
                None
            )
        );
        let mut port = PortStatus::idle(Updater::LinuxPayload);
        port.available = Some(item("0.3.0-rc.1", CATALOG_FORMAT, true));
        port.ready = true;
        let shown = UpdateStatus::compose(Some(&port), &policy, false);
        assert!(shown.available.unwrap().prerelease && shown.ready);
        // A development build's port names its reason and offers nothing.
        let development = UnavailablePort::new(UnavailableReason::DevelopmentBuild).status();
        let status = UpdateStatus::compose(Some(&development), &policy, false);
        assert_eq!(
            serde_json::to_value((status.mechanism, status.reason)).unwrap(),
            json!(["unavailable", "development-build"])
        );
    }

    #[test]
    fn update_a_check_finishes_once_however_it_ends() {
        let wakes = Arc::new(AtomicU64::new(0));
        let counted = wakes.clone();
        let notifier = UpdateNotifier::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        });
        assert!(!notifier.finished(1) && !notifier.take());
        CheckDone::new(notifier.clone(), 1).finish();
        assert!(notifier.finished(1) && !notifier.finished(2));
        assert!(notifier.take() && !notifier.take());
        drop(CheckDone::new(notifier.clone(), 2));
        assert!(notifier.finished(2));
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn update_a_skipped_version_is_checked() {
        for good in ["0.2.0", "0.3.0-rc.1", "1.0.0+build.7"] {
            assert!(check_version(good).is_ok(), "{good}");
        }
        for bad in ["", "0.2.0 ", "../0.2", &"9".repeat(65)] {
            assert!(check_version(bad).is_err(), "{bad}");
        }
    }
}
