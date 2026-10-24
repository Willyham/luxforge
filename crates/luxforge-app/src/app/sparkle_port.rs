//! The macOS update port (`docs/design/auto-update.md#macos-sparkle`): the core's [`UpdatePort`]
//! carried out by Sparkle 2 through `luxforge-sparkle`'s bridge, whose user driver and delegate
//! hand every Sparkle event to this port as a plain value.
//!
//! The port holds the state `update.status` reports and decides every answer; the bridge only
//! carries it to Sparkle on AppKit's main thread. Every [`UpdatePort`] call returns at once: it
//! records what it needs and queues a [`Command`], and Sparkle reads the feed, downloads, verifies
//! and extracts on its own threads.
//!
//! - **Preferences.** Each [`UpdatePort::apply`] sets Sparkle's automatic checks to the person's
//!   `update_checks`, and Sparkle never downloads on its own: an update downloads only when the
//!   person asks to install it or [`UpdatePolicy::installs_automatically`] allows it, item by item.
//!   An update that changes the catalog format is therefore never prepared without being asked
//!   for, whatever Sparkle itself holds. The skip is Luxforge's own: an offer the person skips is
//!   dismissed, never skipped in Sparkle.
//! - **Checks.** Sparkle schedules its daily check itself; the port asks for one more at launch,
//!   10 seconds after the first policy with checks on. `update.check` asks Sparkle to check now, or
//!   joins the check that runs. An offer Sparkle holds is dismissed first, so a check always reads
//!   the feed again; while an update downloads or is ready, there is nothing to check.
//! - **Installing.** `update.install` downloads what is still missing, then posts the desktop's
//!   ordinary close through the same wake signal as the Linux port. Once the owner thread has
//!   joined, so the catalog is closed, [`SparklePort::at_quit`] tells Sparkle to install and
//!   relaunch, and waits for the installer to take the update. A relaunch Sparkle starts before the
//!   catalog is closed is postponed by the delegate until then. An update installed automatically
//!   is left to the installer, which replaces the app once it has terminated, without relaunching;
//!   one no longer wanted is cancelled at quit.
use super::update_port::{AtQuit, feed_update_from};
use luxforge_core::{
    Error,
    update::{
        CheckDone, CheckOutcome, FeedUpdate, LastCheck, PortStatus, Progress, UpdateNotifier,
        UpdatePolicy, UpdatePort, Updater,
    },
};
use luxforge_sparkle::{CheckKind, Choice, Command, Driver, Event, Listener, Reply, Stage};
use serde_json::json;
use std::{
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
#[path = "sparkle_port_tests.rs"]
mod tests;

/// How long after the first policy with checks on the launch's own check waits, so it runs once
/// the workspace is up.
pub(crate) const LAUNCH_CHECK_SECONDS: u64 = 10;
/// How long the quit path waits for the installer to take a requested update.
pub(crate) const INSTALL_WAIT: Duration = Duration::from_secs(20);
/// How long the quit path waits for an unwanted installation to be cancelled.
pub(crate) const CANCEL_WAIT: Duration = Duration::from_secs(5);

/// The reply Sparkle is waiting on, which the port answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Held {
    /// An offer, at this stage.
    Found(Stage),
    /// A downloaded update, ready to install.
    Ready,
}

impl Held {
    fn reply(self) -> Reply {
        match self {
            Self::Found(_) => Reply::Found,
            Self::Ready => Reply::Ready,
        }
    }
}

struct State {
    status: PortStatus,
    notifier: Option<UpdateNotifier>,
    policy: Option<UpdatePolicy>,
    held: Option<Held>,
    downloading: bool,
    /// Bytes received, and the count at the last progress report.
    received: u64,
    reported: u64,
    waiting: Vec<CheckDone>,
    /// Ask Sparkle for a check once the held offer's session has ended.
    check_after_cycle: bool,
    install_requested: bool,
    /// An installation the port asked Sparkle to cancel, until Sparkle dismisses it.
    cancelling: bool,
    /// The delegate postponed a relaunch until the catalog is closed.
    postponed: bool,
    /// The quit path has closed the catalog.
    closed: bool,
    /// The installer took the update.
    installing: bool,
    configured: bool,
}

/// The macOS port ([module documentation](self)).
pub(crate) struct SparklePort {
    shared: Arc<Shared>,
}

struct Shared {
    driver: OnceLock<Arc<dyn Driver>>,
    state: Mutex<State>,
    /// Wakes the quit path's wait.
    changed: Condvar,
    /// Asks the desktop to run its ordinary quit path; must never block.
    quit: Arc<dyn Fn() + Send + Sync>,
}

/// The listener the bridge holds: the port's shared state.
pub(crate) struct PortListener(Arc<Shared>);

impl SparklePort {
    /// A port for this bundle's `build`, whose listener is handed to the bridge before
    /// [`Self::drive`] gives it the bridge.
    pub(crate) fn new(
        build: Option<u64>,
        quit: Arc<dyn Fn() + Send + Sync>,
    ) -> (Arc<Self>, Arc<PortListener>) {
        let shared = Arc::new(Shared {
            driver: OnceLock::new(),
            state: Mutex::new(State {
                status: PortStatus {
                    build,
                    ..PortStatus::idle(Updater::Sparkle)
                },
                notifier: None,
                policy: None,
                held: None,
                downloading: false,
                received: 0,
                reported: 0,
                waiting: Vec::new(),
                check_after_cycle: false,
                install_requested: false,
                cancelling: false,
                postponed: false,
                closed: false,
                installing: false,
                configured: false,
            }),
            changed: Condvar::new(),
            quit,
        });
        (
            Arc::new(Self {
                shared: shared.clone(),
            }),
            Arc::new(PortListener(shared)),
        )
    }

    /// The bridge commands go to. Until it is set, commands are dropped.
    pub(crate) fn drive(&self, driver: Arc<dyn Driver>) {
        let _ = self.shared.driver.set(driver);
    }

    /// The end of the quit path, after the catalog owner has joined. Never called on AppKit's main
    /// thread, which Sparkle needs to finish: there it decides without waiting.
    pub(crate) fn at_quit(&self, install_wait: Duration, cancel_wait: Duration) -> AtQuit {
        let shared = &self.shared;
        let wait = !luxforge_sparkle::on_main_thread();
        let mut state = shared.lock();
        state.closed = true;
        let ready = state.status.ready;
        let automatic = match (&state.policy, &state.status.available) {
            (Some(policy), Some(update)) => policy.installs_automatically(update),
            _ => false,
        };
        if state.downloading {
            state.downloading = false;
            shared.send(Command::CancelDownload);
        }
        if !ready {
            return AtQuit::Nothing;
        }
        if state.install_requested {
            if std::mem::take(&mut state.postponed) {
                shared.send(Command::Resume);
            } else if let Some(held) = state.held.take() {
                shared.send(Command::Answer(held.reply(), Choice::Install));
            }
            if !wait {
                return AtQuit::Failed("the quit path ran on the main thread".into());
            }
            let deadline = Instant::now() + install_wait;
            while !state.installing && state.status.error.is_none() {
                let now = Instant::now();
                if now >= deadline {
                    return AtQuit::Failed("Sparkle's installer did not take the update".into());
                }
                state = shared.wait(state, deadline - now);
            }
            return match &state.status.error {
                Some(error) if !state.installing => AtQuit::Failed(error.clone()),
                // Sparkle's installer replaces the app once this process has exited, and starts
                // the new version.
                _ => AtQuit::Installed,
            };
        }
        if automatic {
            // The installer replaces the app once this process has exited, without relaunching.
            return AtQuit::Installed;
        }
        // Prepared and no longer wanted: cancel it, or it would install at termination.
        if let Some(held) = state.held.take() {
            state.cancelling = true;
            shared.send(Command::Answer(held.reply(), Choice::Skip));
            let deadline = Instant::now() + cancel_wait;
            while wait && state.cancelling && Instant::now() < deadline {
                let left = deadline - Instant::now();
                state = shared.wait(state, left);
            }
        }
        AtQuit::Nothing
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    fn wait<'a>(&self, state: MutexGuard<'a, State>, timeout: Duration) -> MutexGuard<'a, State> {
        self.changed
            .wait_timeout(state, timeout)
            .unwrap_or_else(|poison| poison.into_inner())
            .0
    }

    fn send(&self, command: Command) {
        if let Some(driver) = self.driver.get() {
            driver.send(command);
        }
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// What an event leaves to do once the state's lock is released.
#[derive(Default)]
struct After {
    finish: Vec<CheckDone>,
    quit: bool,
}

impl State {
    fn finish_waiting(&mut self, after: &mut After) {
        after.finish.append(&mut self.waiting);
    }

    fn checked(&mut self, outcome: CheckOutcome) {
        self.status.checking = false;
        self.status.last_check = Some(LastCheck {
            at_ms: now_ms(),
            outcome,
        });
    }

    fn wanted(&self, update: &FeedUpdate) -> bool {
        self.install_requested
            || self
                .policy
                .as_ref()
                .is_some_and(|policy| policy.installs_automatically(update))
    }

    fn start_download(&mut self, shared: &Shared) {
        shared.send(Command::Answer(Reply::Found, Choice::Install));
        self.held = None;
        self.downloading = true;
        self.received = 0;
        self.reported = 0;
        self.status.progress = Some(Progress {
            received: 0,
            total: None,
        });
        self.status.error = None;
    }

    fn event(&mut self, shared: &Shared, event: Event, after: &mut After) {
        match event {
            Event::Failed(error) => {
                self.status.error = Some(error);
                self.status.checking = false;
                self.finish_waiting(after);
            }
            Event::CheckStarted(CheckKind::Asked | CheckKind::Background) => {
                self.status.checking = true;
            }
            Event::CheckStarted(CheckKind::Information) => {}
            Event::Found { item, stage } => {
                let update = feed_update_from(&item);
                self.checked(CheckOutcome::Found);
                self.status.error = None;
                self.finish_waiting(after);
                self.held = Some(Held::Found(stage));
                self.status.ready = stage == Stage::Installing;
                self.status.available = Some(update.clone());
                if stage != Stage::Installing && self.wanted(&update) {
                    self.start_download(shared);
                    if let Some(progress) = &mut self.status.progress {
                        progress.total = Some(item.length).filter(|length| *length > 0);
                    }
                }
            }
            Event::NotFound => {
                self.checked(CheckOutcome::UpToDate);
                self.status.available = None;
                self.status.ready = false;
                self.status.progress = None;
                self.status.error = None;
                self.install_requested = false;
                self.finish_waiting(after);
            }
            Event::Error(error) => {
                if self.status.checking {
                    self.checked(CheckOutcome::Failed);
                }
                self.status.error = Some(error);
                self.status.progress = None;
                self.downloading = false;
                self.install_requested = false;
                self.finish_waiting(after);
            }
            Event::CycleFinished { error } => {
                self.status.checking = false;
                if let Some(error) = error {
                    self.status.error.get_or_insert(error);
                }
                if std::mem::take(&mut self.check_after_cycle) {
                    shared.send(Command::Check);
                    self.status.checking = true;
                } else {
                    self.finish_waiting(after);
                }
            }
            Event::DownloadStarted => {
                self.downloading = true;
                self.received = 0;
                self.reported = 0;
                let total = self.status.progress.and_then(|progress| progress.total);
                self.status.progress = Some(Progress { received: 0, total });
            }
            Event::DownloadLength(length) => {
                self.status.progress = Some(Progress {
                    received: self.received,
                    total: Some(length),
                });
            }
            Event::DownloadReceived(bytes) => {
                self.received = self.received.saturating_add(bytes);
                let total = self.status.progress.and_then(|progress| progress.total);
                // Reported at most once a percent and at least every 256 KiB, as on Linux.
                let step = total.map_or(256 * 1024, |total| (total / 100).max(256 * 1024));
                if self.received - self.reported >= step || Some(self.received) == total {
                    self.reported = self.received;
                    self.status.progress = Some(Progress {
                        received: self.received,
                        total,
                    });
                }
            }
            Event::Extracting => {
                let total = self.status.progress.and_then(|progress| progress.total);
                self.status.progress = Some(Progress {
                    received: total.unwrap_or(self.received),
                    total,
                });
            }
            Event::Ready => {
                self.downloading = false;
                self.held = Some(Held::Ready);
                self.status.progress = None;
                self.status.ready = true;
                after.quit = self.install_requested;
            }
            Event::RelaunchPostponed => {
                self.postponed = true;
                after.quit = !self.closed;
            }
            Event::Installing { .. } => {
                self.installing = true;
            }
            Event::Dismissed => {
                self.held = None;
                self.downloading = false;
                if std::mem::take(&mut self.cancelling) {
                    self.status.ready = false;
                    self.status.progress = None;
                }
            }
            Event::Refused(_) => {
                if self.status.checking && !self.check_after_cycle {
                    self.status.checking = false;
                    self.finish_waiting(after);
                }
            }
        }
    }
}

impl Listener for PortListener {
    fn event(&self, event: Event) {
        let shared = &self.0;
        let progress_only = matches!(event, Event::DownloadReceived(_));
        let mut after = After::default();
        let notifier = {
            let mut state = shared.lock();
            let before = state.status.progress;
            state.event(shared, event, &mut after);
            let moved = !progress_only || state.status.progress != before;
            shared.changed.notify_all();
            state.notifier.clone().filter(|_| moved)
        };
        for done in after.finish {
            done.finish();
        }
        if let Some(notifier) = notifier {
            notifier.changed();
        }
        if after.quit {
            (shared.quit)();
        }
    }

    /// The `prerelease` channel as well as the default one, so a release candidate is offered.
    fn allowed_channels(&self) -> Vec<String> {
        vec!["prerelease".into()]
    }

    fn postpone_relaunch(&self) -> bool {
        !self.0.lock().closed
    }
}

impl UpdatePort for SparklePort {
    fn attach(&self, notifier: UpdateNotifier) {
        self.shared.lock().notifier = Some(notifier);
    }

    fn status(&self) -> PortStatus {
        self.shared.lock().status.clone()
    }

    fn apply(&self, policy: &UpdatePolicy) {
        let shared = &self.shared;
        let mut state = shared.lock();
        shared.send(Command::Configure {
            automatic_checks: policy.checks,
        });
        if !std::mem::replace(&mut state.configured, true) && policy.checks {
            shared.send(Command::CheckInBackgroundAfter(LAUNCH_CHECK_SECONDS));
        }
        state.policy = Some(policy.clone());
        let wanted = state
            .status
            .available
            .clone()
            .is_some_and(|update| state.wanted(&update));
        match state.held {
            Some(Held::Found(Stage::NotDownloaded | Stage::Downloaded)) if wanted => {
                state.start_download(shared);
            }
            _ => {}
        }
        if state.downloading && !wanted {
            // A background download the policy no longer wants stops.
            state.downloading = false;
            state.status.progress = None;
            shared.send(Command::CancelDownload);
        }
    }

    fn check(&self, done: CheckDone) {
        let shared = &self.shared;
        let mut state = shared.lock();
        if state.closed {
            return;
        }
        if state.status.checking {
            state.waiting.push(done);
            return;
        }
        if state.downloading || state.status.ready || state.install_requested {
            // An update is on its way: there is nothing to check until it is installed.
            drop(state);
            done.finish();
            return;
        }
        state.waiting.push(done);
        state.status.checking = true;
        if let Some(Held::Found(_)) = state.held.take() {
            // Sparkle holds an offer, and checks again only once its session ends.
            state.check_after_cycle = true;
            shared.send(Command::Answer(Reply::Found, Choice::Dismiss));
        } else {
            shared.send(Command::Check);
        }
    }

    fn request_install(&self) -> Result<(), Error> {
        let shared = &self.shared;
        let mut state = shared.lock();
        if state.status.available.is_none() {
            return Err(Error::not_ready("no update is available")
                .with_data(json!({"reason": "no-update"})));
        }
        state.install_requested = true;
        match state.held {
            Some(Held::Ready | Held::Found(Stage::Installing)) => {
                drop(state);
                (shared.quit)();
            }
            Some(Held::Found(_)) => state.start_download(shared),
            None if state.downloading => {}
            None => {
                // Sparkle's session ended with the offer: find it again, and install it then.
                state.status.checking = true;
                shared.send(Command::Check);
            }
        }
        Ok(())
    }

    fn skip(&self, version: &str) {
        let shared = &self.shared;
        let mut state = shared.lock();
        if state.install_requested
            || state
                .status
                .available
                .as_ref()
                .is_none_or(|update| update.version != version)
        {
            return;
        }
        match state.held.take() {
            Some(Held::Found(Stage::NotDownloaded | Stage::Downloaded)) => {
                shared.send(Command::Answer(Reply::Found, Choice::Dismiss));
            }
            Some(held) => {
                // Prepared for the installer: cancel the installation.
                state.cancelling = true;
                shared.send(Command::Answer(held.reply(), Choice::Skip));
            }
            None => {}
        }
        if std::mem::take(&mut state.downloading) {
            shared.send(Command::CancelDownload);
        }
        state.status.progress = None;
        let notifier = state.notifier.clone();
        drop(state);
        if let Some(notifier) = notifier {
            notifier.changed();
        }
    }

    /// The Sparkle port runs only in Applications (outside them a launch gets the update port's
    /// move port), so it never moves.
    fn move_to_applications(&self, _replace: bool) -> Result<(), Error> {
        Err(
            Error::not_ready("this installation is in Applications already")
                .with_data(json!({ "reason": "not-supported" })),
        )
    }
}
