//! The desktop's update port (`docs/design/auto-update.md`): which mechanism a launch gets, and
//! the Linux payload updater's port, [`LinuxPayloadPort`], which carries out the core's `update.*`
//! commands with `luxforge_update::payload`.
//!
//! **Selection** ([`select`]). A Linux release build (one compiled with `LUXFORGE_RELEASE_VERSION`)
//! launched by a person gets the payload port, with the feed base URL and public key of the
//! `release/update.json` it was compiled with; without a committed key it reports `no-update-key`.
//! Every other launch gets an [`UnavailablePort`]: development and test builds, and hidden,
//! evidence and test-receiver launches, which is how smoke, verify and background runs start, so
//! none of them ever contacts the real feed. A debug Linux build may be pointed at a local feed and
//! key for a manual test ([`DEV_FEED`], [`DEV_KEY`]); a release build never reads them.
//!
//! **The port.** Every [`UpdatePort`] call returns at once. One worker thread runs checks: one
//! shortly after launch, then one every day while `update_checks` is on, and each `update.check`.
//! It sleeps on a condition variable with the next check's deadline, so an idle port wakes once a
//! day. A download runs on its own thread, one at a time, when the person asks to install or the
//! policy installs an update automatically; it reports its progress at most once a percent.
//!
//! **Installing** goes through the ordinary quit path. `update.install` downloads what it still
//! needs, then posts [`Message::Close`] through this module's wake signal. The close stops the live
//! server and the catalog owner, and only once the owner thread has joined, so the catalog is
//! closed, does it call [`at_quit`], which swaps the payload and execs the new executable with the
//! launch's arguments. An update the policy installs automatically is swapped at an ordinary quit
//! without relaunching. Quitting cancels a check or download still running.
use super::{Editor, message::Message, waker::Signal};
use crate::Config;
use iced::Subscription;
use luxforge_core::{
    Error,
    capabilities::{
        endpoint::{EndpointClass, parse_endpoint},
        transport::{Method, RedirectPolicy, SendOptions, Transport, TransportRequest},
    },
    jobs::JobControl,
    update::{
        CheckDone, CheckOutcome, FeedUpdate, LastCheck, PortStatus, Progress, UnavailablePort,
        UnavailableReason, UpdateNotifier, UpdatePolicy, UpdatePort, Updater,
    },
};
use luxforge_update::{
    AppcastItem, Channel, PublicKey, UpdateConfig,
    payload::{Cancel, Fetch, FetchKind, FetchRequest, Mode, Payload, Staged},
};
use serde_json::json;
use std::{
    ffi::OsString,
    io::Write,
    path::PathBuf,
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock},
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(test)]
#[path = "update_port_tests.rs"]
mod tests;

/// `release/update.json` as this build was compiled with: the feed base URL and the public key.
const RELEASE_UPDATE_CONFIG: &str = include_str!("../../../../release/update.json");

/// The version a tag build compiled in; development builds have none.
const RELEASE_VERSION: Option<&str> = option_env!("LUXFORGE_RELEASE_VERSION");

/// A debug Linux build's local feed base URL, for a manual test of the payload updater: `http` or
/// `https` ending in `/`, on this machine or remote. Read only with [`DEV_KEY`], and never by a
/// release build.
pub(crate) const DEV_FEED: &str = "LUXFORGE_UPDATE_DEV_FEED";
/// The base64 public key a [`DEV_FEED`]'s archives are signed with.
pub(crate) const DEV_KEY: &str = "LUXFORGE_UPDATE_DEV_KEY";

/// How long after launch the first automatic check waits, so it runs once the workspace is up.
const FIRST_CHECK: Duration = Duration::from_secs(10);
/// Automatic checks after the first, measured from the last check that finished.
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const READ_TIMEOUT: Duration = Duration::from_secs(60);
const FEED_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// Where a release archive's download may be redirected: GitHub's release download stores.
const ARCHIVE_ORIGINS: [&str; 3] = [
    "https://github.com",
    "https://objects.githubusercontent.com",
    "https://release-assets.githubusercontent.com",
];
/// The `data.reason` of a refusal from an installation that can only offer the release page.
const NOTIFY_ONLY: &str = "notify-only";

/// What a launch's update port is.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Choice {
    Unavailable(UnavailableReason),
    Payload {
        feed_base_url: String,
        key: PublicKey,
        classes: &'static [EndpointClass],
    },
}

/// A development feed and key from the environment.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DevFeed {
    feed_base_url: String,
    key: PublicKey,
}

/// The development feed the environment names, when both variables are set and well formed.
fn dev_feed(feed: Option<OsString>, key: Option<OsString>) -> Option<DevFeed> {
    let feed = feed?.into_string().ok()?;
    let key = PublicKey::from_base64(key?.to_str()?).ok()?;
    let valid = (feed.starts_with("https://") || feed.starts_with("http://"))
        && feed.ends_with('/')
        && !feed.contains(['?', '#']);
    valid.then_some(DevFeed {
        feed_base_url: feed,
        key,
    })
}

/// The choice for a launch: a release build or not, launched by a person or automated, on Linux or
/// not, with the embedded configuration and any development feed.
fn choose(
    release: bool,
    automated: bool,
    linux: bool,
    embedded: &str,
    development: Option<DevFeed>,
) -> Choice {
    if automated {
        return Choice::Unavailable(UnavailableReason::DevelopmentBuild);
    }
    if release {
        // The release's own configuration; a release never takes another feed or key.
        let config = UpdateConfig::parse(embedded).ok();
        let Some((feed_base_url, Some(key))) =
            config.map(|config| (config.feed_base_url, config.public_key))
        else {
            return Choice::Unavailable(UnavailableReason::NoUpdateKey);
        };
        if !linux {
            // macOS updates through Sparkle, whose port is selected here once it is built
            // (`docs/design/auto-update.md#macos-sparkle`); Windows has no updater yet.
            return Choice::Unavailable(UnavailableReason::DevelopmentBuild);
        }
        return Choice::Payload {
            feed_base_url,
            key,
            classes: &[EndpointClass::Remote],
        };
    }
    match development {
        Some(DevFeed { feed_base_url, key }) if linux => Choice::Payload {
            feed_base_url,
            key,
            classes: &[EndpointClass::Remote, EndpointClass::Loopback],
        },
        _ => Choice::Unavailable(UnavailableReason::DevelopmentBuild),
    }
}

/// The update port for this launch ([module documentation](self)). A payload port it creates is
/// also the one [`at_quit`] installs through.
pub(super) fn select(config: &Config, transport: Arc<dyn Transport>) -> Arc<dyn UpdatePort> {
    let automated = config.hidden || config.evidence.is_some() || config.usage_receiver.is_some();
    let linux = cfg!(target_os = "linux");
    let development = if cfg!(debug_assertions) && linux {
        dev_feed(std::env::var_os(DEV_FEED), std::env::var_os(DEV_KEY))
    } else {
        None
    };
    let release = RELEASE_VERSION.is_some() && !cfg!(test);
    let located = || {
        std::env::current_exe()
            .ok()
            .and_then(|executable| Payload::locate(&executable).ok())
    };
    match choose(
        release,
        automated,
        linux,
        RELEASE_UPDATE_CONFIG,
        development,
    ) {
        Choice::Unavailable(reason) => Arc::new(UnavailablePort {
            reason,
            build: if release && linux {
                located().map(|payload| payload.build().build)
            } else {
                None
            },
        }),
        Choice::Payload {
            feed_base_url,
            key,
            classes,
        } => {
            // A binary outside a payload, such as one `cargo` built, is a development build.
            let Some(payload) = located() else {
                return Arc::new(UnavailablePort::new(UnavailableReason::DevelopmentBuild));
            };
            let port = LinuxPayloadPort::start(
                payload,
                PortSettings {
                    feed_base_url,
                    key,
                    fetch: Arc::new(HttpFetch { transport, classes }),
                    quit: Arc::new(|| quit_signal().post()),
                    desktop_entry: luxforge_update::payload::user_desktop_entry(
                        std::env::var_os("XDG_DATA_HOME").as_deref(),
                        std::env::var_os("HOME").as_deref(),
                    ),
                    arguments: std::env::args_os().skip(1).collect(),
                    first_check: FIRST_CHECK,
                    interval: CHECK_INTERVAL,
                },
            );
            let _ = INSTALLED.set(port.clone());
            port
        }
    }
}

/// The payload port this launch installs through at quit, when it has one.
static INSTALLED: OnceLock<Arc<LinuxPayloadPort>> = OnceLock::new();
/// The port's request for the ordinary quit path.
static QUIT: OnceLock<Signal<Message>> = OnceLock::new();

fn quit_signal() -> &'static Signal<Message> {
    QUIT.get_or_init(|| Signal::new(|| Message::Close))
}

/// The port's quit request, carried into the update loop as [`Message::Close`]. Only a launch with
/// a payload port listens; the blocked stream has no timer.
pub(super) fn subscription(_: &Editor) -> Subscription<Message> {
    if INSTALLED.get().is_some() {
        Subscription::run(|| quit_signal().stream())
    } else {
        Subscription::none()
    }
}

/// The end of the quit path, after the catalog owner has joined: install a staged update the person
/// asked for, or one the policy installs automatically, and exec the new version for the first.
/// Returns when there is nothing to install, after an automatic install, or when the relaunch
/// failed; the app then exits as usual.
pub(super) fn at_quit() {
    let Some(port) = INSTALLED.get() else {
        return;
    };
    match port.at_quit() {
        AtQuit::Relaunch {
            executable,
            arguments,
        } => {
            let error = luxforge_update::payload::relaunch(&executable, &arguments);
            eprintln!("Luxforge was updated, but the new version could not start: {error}");
        }
        AtQuit::Failed(why) => eprintln!("Luxforge could not install its update: {why}"),
        AtQuit::Installed | AtQuit::Nothing => {}
    }
}

/// The desktop's checked HTTPS transport as the payload updater's fetch.
struct HttpFetch {
    transport: Arc<dyn Transport>,
    classes: &'static [EndpointClass],
}

impl Fetch for HttpFetch {
    fn fetch(&self, request: FetchRequest<'_>, sink: &mut dyn Write) -> Result<(), String> {
        let endpoint =
            parse_endpoint(request.url, self.classes).map_err(|error| error.to_string())?;
        let control = JobControl::new();
        let stop = control.clone();
        request
            .cancel
            .on_cancel(move || stop.cancel("the update was cancelled"));
        let (origins, total_timeout): (Vec<String>, _) = match request.kind {
            FetchKind::Feed => (vec![endpoint.origin()], FEED_TIMEOUT),
            FetchKind::Archive => (
                ARCHIVE_ORIGINS
                    .iter()
                    .map(|origin| (*origin).to_owned())
                    .collect(),
                ARCHIVE_TIMEOUT,
            ),
        };
        let response = self
            .transport
            .send(
                &TransportRequest {
                    method: Method::Get,
                    endpoint,
                    headers: Vec::new(),
                    body: Vec::new(),
                },
                SendOptions {
                    max_request_bytes: 0,
                    max_response_bytes: request.max_bytes,
                    connect_timeout: CONNECT_TIMEOUT,
                    read_timeout: READ_TIMEOUT,
                    total_timeout,
                    redirects: RedirectPolicy {
                        max: 3,
                        origins: &origins,
                    },
                    control: &control,
                    progress: &mut |_, _| {},
                    head: None,
                },
                sink,
            )
            .map_err(|error| error.to_string())?;
        match response.status {
            200 => Ok(()),
            status => Err(format!("the server answered HTTP {status}")),
        }
    }
}

/// What the port needs besides its payload.
pub(crate) struct PortSettings {
    pub(crate) feed_base_url: String,
    pub(crate) key: PublicKey,
    pub(crate) fetch: Arc<dyn Fetch>,
    /// Asks the desktop to run its ordinary quit path; must never block.
    pub(crate) quit: Arc<dyn Fn() + Send + Sync>,
    /// The person's desktop entry, refreshed after a swap when it points at this payload.
    pub(crate) desktop_entry: Option<PathBuf>,
    /// The launch's arguments, which the new version is started with.
    pub(crate) arguments: Vec<OsString>,
    pub(crate) first_check: Duration,
    pub(crate) interval: Duration,
}

/// What the quit path does with a staged update.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AtQuit {
    Nothing,
    /// Installed automatically; the app exits as usual.
    Installed,
    /// Installed on request: exec the new executable.
    Relaunch {
        executable: PathBuf,
        arguments: Vec<OsString>,
    },
    /// The install failed; the installed payload is unchanged and the staging removed.
    Failed(String),
}

/// The Linux payload updater's port ([module documentation](self)).
pub(crate) struct LinuxPayloadPort {
    shared: Arc<Shared>,
}

struct Shared {
    payload: Payload,
    /// `linux-payload` or `linux-notify`, decided once as the port starts.
    updater: Updater,
    settings: PortSettings,
    state: Mutex<State>,
    /// Wakes the worker.
    wake: Condvar,
}

#[derive(Default)]
struct State {
    status: Option<PortStatus>,
    notifier: Option<UpdateNotifier>,
    policy: Option<UpdatePolicy>,
    /// The feed item [`PortStatus::available`] describes.
    item: Option<AppcastItem>,
    /// That item, downloaded, verified and extracted.
    staged: Option<Staged>,
    /// Staged updates no longer wanted, removed by the worker.
    discard: Vec<Staged>,
    /// `update.check` calls waiting for the running or next check.
    waiting: Vec<CheckDone>,
    check_requested: bool,
    /// The running check's cancel.
    checking: Option<Cancel>,
    clean_requested: bool,
    /// The running download's build and cancel.
    download: Option<(u64, Cancel)>,
    download_thread: Option<JoinHandle<()>>,
    install_requested: bool,
    /// When an owner first attached: the launch the first automatic check counts from.
    started: Option<Instant>,
    last_finished: Option<Instant>,
    stopped: bool,
}

impl LinuxPayloadPort {
    /// The port for `payload`, whose worker starts now and waits for an owner to attach.
    pub(crate) fn start(payload: Payload, settings: PortSettings) -> Arc<Self> {
        let updater = match payload.mode() {
            Mode::Payload => Updater::LinuxPayload,
            Mode::Notify(_) => Updater::LinuxNotify,
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                status: Some(PortStatus {
                    build: Some(payload.build().build),
                    ..PortStatus::idle(updater)
                }),
                ..State::default()
            }),
            payload,
            updater,
            settings,
            wake: Condvar::new(),
        });
        let worker = shared.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("luxforge-update".into())
            .spawn(move || worker.work())
        {
            shared.lock().status_mut().error =
                Some(format!("The updater could not start: {error}"));
        }
        Arc::new(Self { shared })
    }

    /// The end of the quit path: stop checking and downloading, then install a staged update the
    /// person asked for or the policy installs automatically. Call it once, after the catalog
    /// owner has joined.
    pub(crate) fn at_quit(&self) -> AtQuit {
        let shared = &self.shared;
        let thread = {
            let mut state = shared.lock();
            state.stop();
            shared.wake.notify_all();
            state.download_thread.take()
        };
        // Cancelled, so it ends at its next write or step.
        if let Some(thread) = thread {
            let _ = thread.join();
        }
        let (staged, requested, automatic) = {
            let mut state = shared.lock();
            let Some(staged) = state.staged.take() else {
                return AtQuit::Nothing;
            };
            let automatic = match (&state.policy, &state.status_ref().available) {
                (Some(policy), Some(update)) if update.build == staged.build() => {
                    policy.installs_automatically(update)
                }
                _ => false,
            };
            (staged, state.install_requested, automatic)
        };
        if !requested && !automatic {
            return AtQuit::Nothing;
        }
        let payload = &shared.payload;
        match payload.install(&staged, shared.settings.desktop_entry.as_deref()) {
            Ok(installed) if requested => AtQuit::Relaunch {
                executable: installed.executable,
                arguments: shared.settings.arguments.clone(),
            },
            Ok(_) => AtQuit::Installed,
            Err(error) => {
                let _ = payload.discard(&staged);
                AtQuit::Failed(error.to_string())
            }
        }
    }
}

impl Drop for LinuxPayloadPort {
    fn drop(&mut self) {
        self.shared.lock().stop();
        self.shared.wake.notify_all();
    }
}

impl State {
    fn status_ref(&self) -> &PortStatus {
        self.status.as_ref().expect("the status is set at start")
    }

    fn status_mut(&mut self) -> &mut PortStatus {
        self.status.as_mut().expect("the status is set at start")
    }

    /// Stop scheduling and cancel whatever runs.
    fn stop(&mut self) {
        self.stopped = true;
        if let Some(cancel) = self.checking.take() {
            cancel.cancel();
        }
        if let Some((_, cancel)) = self.download.take() {
            cancel.cancel();
        }
    }

    /// Cancel the download and drop what is staged, as when the offer goes away.
    fn forget_download(&mut self) {
        if let Some((_, cancel)) = self.download.take() {
            cancel.cancel();
        }
        if let Some(staged) = self.staged.take() {
            self.discard.push(staged);
        }
        let status = self.status_mut();
        status.ready = false;
        status.progress = None;
    }
}

fn notify(notifier: Option<UpdateNotifier>) {
    if let Some(notifier) = notifier {
        notifier.changed();
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// The core's view of a feed item; the payload updater only offers items with a catalog format.
fn feed_update(item: &AppcastItem) -> FeedUpdate {
    FeedUpdate {
        version: item.version.clone(),
        build: item.build,
        notes_url: item.link.clone(),
        catalog_format: item.catalog_format.map_or(-1, i64::from),
        prerelease: item.channel.as_deref() == Channel::Prerelease.sparkle_name(),
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// The worker: cleanup after launch, discarded stages, requested and scheduled checks, asleep
    /// in between.
    fn work(self: Arc<Self>) {
        let mut state = self.lock();
        loop {
            if state.stopped {
                return;
            }
            if std::mem::take(&mut state.clean_requested) {
                drop(state);
                if let Err(error) = self.payload.clean_after_launch() {
                    eprintln!("Luxforge could not remove its previous version: {error}");
                }
                state = self.lock();
                continue;
            }
            if !state.discard.is_empty() {
                let discard = std::mem::take(&mut state.discard);
                drop(state);
                for staged in discard {
                    let _ = self.payload.discard(&staged);
                }
                state = self.lock();
                continue;
            }
            let due = self.due(&state);
            let now = Instant::now();
            if std::mem::take(&mut state.check_requested) || due.is_some_and(|due| due <= now) {
                state = self.run_check(state);
                continue;
            }
            state = match due {
                Some(due) => {
                    self.wake
                        .wait_timeout(state, due - now)
                        .unwrap_or_else(|poison| poison.into_inner())
                        .0
                }
                None => self
                    .wake
                    .wait(state)
                    .unwrap_or_else(|poison| poison.into_inner()),
            };
        }
    }

    /// When the next automatic check is due: none while checks are off or before an owner attached.
    fn due(&self, state: &State) -> Option<Instant> {
        let policy = state.policy.as_ref()?;
        let started = state.started?;
        if !policy.checks {
            return None;
        }
        Some(match state.last_finished {
            Some(finished) => finished + self.settings.interval,
            None => started + self.settings.first_check,
        })
    }

    /// One check, with the lock released while it reads the feed.
    fn run_check<'a>(
        self: &'a Arc<Self>,
        mut state: MutexGuard<'a, State>,
    ) -> MutexGuard<'a, State> {
        let mut followed = vec![Channel::Default];
        if state
            .policy
            .as_ref()
            .is_some_and(|policy| policy.prerelease_channel)
        {
            followed.push(Channel::Prerelease);
        }
        let cancel = Cancel::new();
        state.checking = Some(cancel.clone());
        state.status_mut().checking = true;
        let notifier = state.notifier.clone();
        drop(state);
        notify(notifier);
        let checked = self.payload.check(
            &*self.settings.fetch,
            &self.settings.feed_base_url,
            &followed,
            &cancel,
        );
        let mut state = self.lock();
        state.checking = None;
        state.status_mut().checking = false;
        state.last_finished = Some(Instant::now());
        let outcome = match checked {
            Ok(checked) => Some(self.adopt(&mut state, checked.item)),
            Err(luxforge_update::Error::Cancelled) => None,
            Err(error) => {
                state.status_mut().error = Some(format!("The update check failed: {error}"));
                Some(CheckOutcome::Failed)
            }
        };
        if let Some(outcome) = outcome {
            state.status_mut().last_check = Some(LastCheck {
                at_ms: now_ms(),
                outcome,
            });
        }
        let done = std::mem::take(&mut state.waiting);
        self.download_if_wanted(&mut state);
        let notifier = state.notifier.clone();
        drop(state);
        for done in done {
            done.finish();
        }
        notify(notifier);
        self.lock()
    }

    /// Take what a check found as the offer.
    fn adopt(&self, state: &mut State, item: Option<AppcastItem>) -> CheckOutcome {
        let Some(item) = item else {
            state.forget_download();
            state.item = None;
            state.install_requested = false;
            let status = state.status_mut();
            status.available = None;
            status.error = None;
            return CheckOutcome::UpToDate;
        };
        if state.item.as_ref().map(|known| known.build) != Some(item.build) {
            state.forget_download();
        }
        let status = state.status_mut();
        status.available = Some(feed_update(&item));
        status.error = None;
        state.item = Some(item);
        CheckOutcome::Found
    }

    /// Start a download when the person asked to install, or the policy installs the offer
    /// automatically.
    fn download_if_wanted(self: &Arc<Self>, state: &mut State) {
        let automatic = match (&state.policy, &state.status_ref().available) {
            (Some(policy), Some(update)) => policy.installs_automatically(update),
            _ => false,
        };
        if state.install_requested || automatic {
            self.start_download(state);
        }
    }

    /// Download the offered item on a thread of its own, unless it is downloading or staged.
    fn start_download(self: &Arc<Self>, state: &mut State) {
        if self.updater != Updater::LinuxPayload || state.stopped {
            return;
        }
        let Some(item) = state.item.clone() else {
            return;
        };
        if state
            .staged
            .as_ref()
            .is_some_and(|staged| staged.build() == item.build)
            || state
                .download
                .as_ref()
                .is_some_and(|(build, _)| *build == item.build)
        {
            return;
        }
        if let Some((_, cancel)) = state.download.take() {
            cancel.cancel();
        }
        let cancel = Cancel::new();
        state.download = Some((item.build, cancel.clone()));
        let status = state.status_mut();
        status.progress = Some(Progress {
            received: 0,
            total: Some(item.enclosure.length),
        });
        status.error = None;
        // One download thread at a time: the next joins the cancelled one first.
        let previous = state.download_thread.take();
        let shared = self.clone();
        let spawned = std::thread::Builder::new()
            .name("luxforge-update-download".into())
            .spawn(move || {
                if let Some(previous) = previous {
                    let _ = previous.join();
                }
                shared.download(&item, &cancel);
            });
        match spawned {
            Ok(thread) => state.download_thread = Some(thread),
            Err(error) => {
                state.download = None;
                let status = state.status_mut();
                status.progress = None;
                status.error = Some(format!("The update could not be downloaded: {error}"));
            }
        }
    }

    /// Download, verify and extract `item`, then offer it as ready, and quit to install it when
    /// the person asked.
    fn download(&self, item: &AppcastItem, cancel: &Cancel) {
        let notifier = self.lock().notifier.clone();
        let step = (item.enclosure.length / 100).max(256 * 1024);
        let mut reported = 0;
        let prepared = self.payload.prepare(
            &*self.settings.fetch,
            item,
            &self.settings.key,
            cancel,
            &mut |received, total| {
                if received - reported >= step || received == total {
                    reported = received;
                    self.lock().status_mut().progress = Some(Progress {
                        received,
                        total: Some(total),
                    });
                    notify(notifier.clone());
                }
            },
        );
        let mut state = self.lock();
        let current = !cancel.is_cancelled()
            && state
                .download
                .as_ref()
                .is_some_and(|(build, _)| *build == item.build);
        if current {
            state.download = None;
            state.status_mut().progress = None;
        }
        let mut quit = false;
        match prepared {
            Ok(staged) if current => {
                state.staged = Some(staged);
                state.status_mut().ready = true;
                quit = state.install_requested;
            }
            Ok(staged) => {
                state.discard.push(staged);
                self.wake.notify_all();
            }
            Err(luxforge_update::Error::Cancelled) => {}
            Err(error) => {
                if current {
                    state.install_requested = false;
                    state.status_mut().error =
                        Some(format!("The update could not be downloaded: {error}"));
                }
            }
        }
        let notifier = state.notifier.clone();
        drop(state);
        notify(notifier);
        if quit {
            (self.settings.quit)();
        }
    }
}

impl UpdatePort for LinuxPayloadPort {
    fn attach(&self, notifier: UpdateNotifier) {
        let mut state = self.shared.lock();
        state.notifier = Some(notifier);
        if state.started.is_none() {
            state.started = Some(Instant::now());
            // The owner has opened the catalog: this launch succeeded, so the previous version and
            // any staging can go.
            state.clean_requested = self.shared.updater == Updater::LinuxPayload;
        }
        self.shared.wake.notify_all();
    }

    fn status(&self) -> PortStatus {
        self.shared.lock().status_ref().clone()
    }

    fn apply(&self, policy: &UpdatePolicy) {
        let mut state = self.shared.lock();
        state.policy = Some(policy.clone());
        // A background download the policy no longer wants stops.
        let wanted = state.install_requested
            || state
                .status_ref()
                .available
                .as_ref()
                .is_some_and(|update| policy.installs_automatically(update));
        if !wanted && let Some((_, cancel)) = state.download.take() {
            cancel.cancel();
            state.status_mut().progress = None;
        }
        self.shared.download_if_wanted(&mut state);
        self.shared.wake.notify_all();
    }

    fn check(&self, done: CheckDone) {
        let mut state = self.shared.lock();
        if state.stopped {
            return;
        }
        state.waiting.push(done);
        if state.checking.is_none() {
            state.check_requested = true;
            self.shared.wake.notify_all();
        }
    }

    fn request_install(&self) -> Result<(), Error> {
        let mut state = self.shared.lock();
        if self.shared.updater == Updater::LinuxNotify {
            return Err(Error::not_ready(
                "this installation cannot replace itself; open the release page instead",
            )
            .with_data(json!({ "reason": NOTIFY_ONLY })));
        }
        if state.item.is_none() {
            return Err(Error::not_ready("no update is available")
                .with_data(json!({ "reason": "no-update" })));
        }
        state.install_requested = true;
        if state.status_ref().ready {
            drop(state);
            (self.shared.settings.quit)();
            return Ok(());
        }
        self.shared.start_download(&mut state);
        Ok(())
    }

    fn skip(&self, version: &str) {
        let mut state = self.shared.lock();
        if state.install_requested
            || state
                .item
                .as_ref()
                .is_none_or(|item| item.version != version)
        {
            return;
        }
        state.forget_download();
        let notifier = state.notifier.clone();
        self.shared.wake.notify_all();
        drop(state);
        notify(notifier);
    }

    fn move_to_applications(&self) -> Result<(), Error> {
        Err(
            Error::not_ready("moving into Applications is for macOS only")
                .with_data(json!({ "reason": "not-supported" })),
        )
    }
}
