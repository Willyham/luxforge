//! `--update-fixture FILE`: a scripted update port for an evidence launch, so the offer, its
//! variants and Settings › General's Updates group can be captured in a real background launch
//! without a feed, a download or a platform updater. It touches no network and no file after the
//! launch has read its script, and only an evidence launch honours it (`main.rs` refuses it
//! without `--evidence-dir`): an ordinary launch keeps the port the platform gives it.
//!
//! The script is one JSON object, every field optional:
//!
//! ```json
//! {"mechanism": "sparkle", "reason": null, "build": 41,
//!  "release": {"version": "0.2.0", "build": 42, "catalog_format": 2, "prerelease": false,
//!              "notes_url": "https://github.com/Willyham/luxforge/releases/tag/v0.2.0"},
//!  "progress": {"received": 4500000, "total": 10000000}, "ready": false,
//!  "launch_check": true,
//!  "refuse_install": {"code": "conflict", "message": "…", "data": {"jobs": […]}}}
//! ```
//!
//! - `mechanism` is what `update.status` names (`sparkle`, `linux-payload`, `linux-notify` or
//!   `unavailable`, default `sparkle`); an unavailable one names its `reason`.
//! - `release` is what every check finds; without it a check finds nothing newer. Its
//!   `catalog_format` defaults to this build's, so a different one is a catalog format change.
//! - `progress` and `ready` are what the port reports of the download once a check has found the
//!   release.
//! - A check ends as soon as it starts, on the owner's thread, as the core's own fake port's does:
//!   the fixture starts no thread and waits for nothing. With `launch_check` (default true) the
//!   port checks once as the owner first applies a policy with checks on, as a release port's
//!   schedule does at launch.
//! - `refuse_install` is the port's own refusal of the next `update.install`, `conflict` or
//!   `not-ready`, with the data the core would give it. An install the port accepts is recorded
//!   and does nothing else: the evidence launch keeps running.
use luxforge_core::{
    Error, ErrorKind,
    update::{
        CATALOG_FORMAT, CheckDone, CheckOutcome, FeedUpdate, LastCheck, PortStatus, Progress,
        UnavailableReason, UpdateNotifier, UpdatePolicy, UpdatePort, Updater,
    },
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
};

/// The script, as the file holds it.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateFixture {
    #[serde(default)]
    mechanism: Option<String>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    build: Option<u64>,
    #[serde(default)]
    release: Option<Release>,
    #[serde(default)]
    progress: Option<Progress>,
    #[serde(default)]
    ready: bool,
    #[serde(default)]
    launch_check: Option<bool>,
    #[serde(default)]
    refuse_install: Option<Refusal>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Release {
    version: String,
    build: u64,
    #[serde(default)]
    notes_url: Option<String>,
    #[serde(default)]
    catalog_format: Option<i64>,
    #[serde(default)]
    prerelease: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Refusal {
    code: String,
    message: String,
    #[serde(default)]
    data: Option<Value>,
}

impl UpdateFixture {
    /// Read and check a script.
    pub(crate) fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read the update fixture: {}", error.kind()))?;
        Self::parse(&text)
    }

    /// Parse and check a script's text.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let fixture: Self = serde_json::from_str(text)
            .map_err(|error| format!("--update-fixture is not a valid script: {error}"))?;
        fixture.updater()?;
        fixture.refusal_kind()?;
        Ok(fixture)
    }

    fn updater(&self) -> Result<Updater, String> {
        Ok(match self.mechanism.as_deref().unwrap_or("sparkle") {
            "sparkle" => Updater::Sparkle,
            "linux-payload" => Updater::LinuxPayload,
            "linux-notify" => Updater::LinuxNotify,
            "unavailable" => {
                let reasons = [
                    UnavailableReason::DevelopmentBuild,
                    UnavailableReason::Headless,
                    UnavailableReason::NoUpdateKey,
                    UnavailableReason::OutsideApplications,
                    UnavailableReason::ReadOnlyInstall,
                ];
                let named = self.reason.as_deref().unwrap_or("development-build");
                let reason = reasons
                    .into_iter()
                    .find(|reason| reason.as_str() == named)
                    .ok_or_else(|| format!("--update-fixture names no reason {named:?}"))?;
                Updater::Unavailable(reason)
            }
            other => return Err(format!("--update-fixture names no mechanism {other:?}")),
        })
    }

    fn refusal_kind(&self) -> Result<Option<ErrorKind>, String> {
        self.refuse_install
            .as_ref()
            .map(|refusal| match refusal.code.as_str() {
                "conflict" => Ok(ErrorKind::Conflict),
                "not-ready" => Ok(ErrorKind::NotReady),
                other => Err(format!(
                    "--update-fixture refuse_install code must be conflict or not-ready, not {other:?}"
                )),
            })
            .transpose()
    }

    /// What every check finds.
    fn found(&self) -> Option<FeedUpdate> {
        self.release.as_ref().map(|release| FeedUpdate {
            version: release.version.clone(),
            build: release.build,
            notes_url: release.notes_url.clone(),
            catalog_format: release.catalog_format.unwrap_or(CATALOG_FORMAT),
            prerelease: release.prerelease,
        })
    }
}

struct State {
    status: PortStatus,
    notifier: Option<UpdateNotifier>,
    /// The launch check has been started.
    launched: bool,
    refusal: Option<Error>,
    installs: usize,
}

struct Inner {
    fixture: UpdateFixture,
    state: Mutex<State>,
}

/// The scripted port: see the module's documentation.
#[derive(Clone)]
pub(crate) struct FixturePort(Arc<Inner>);

impl FixturePort {
    pub(crate) fn new(fixture: UpdateFixture) -> Self {
        let updater = fixture.updater().unwrap_or(Updater::Sparkle);
        let refusal = fixture
            .refusal_kind()
            .ok()
            .flatten()
            .zip(fixture.refuse_install.as_ref())
            .map(|(kind, refusal)| {
                let error = Error::new(kind, refusal.message.clone());
                match &refusal.data {
                    Some(data) => error.with_data(data.clone()),
                    None => error,
                }
            });
        Self(Arc::new(Inner {
            state: Mutex::new(State {
                status: PortStatus {
                    build: fixture.build,
                    ..PortStatus::idle(updater)
                },
                notifier: None,
                launched: false,
                refusal,
                installs: 0,
            }),
            fixture,
        }))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Signal the owner that the status changed. Never blocks.
    fn changed(&self) {
        let notifier = self.state().notifier.clone();
        if let Some(notifier) = notifier {
            notifier.changed();
        }
    }

    /// Run one check: it ends at once with what the fixture finds, and `done` is finished after
    /// the status reports the outcome.
    fn run_check(&self, done: Option<CheckDone>) {
        self.conclude();
        self.changed();
        if let Some(done) = done {
            done.finish();
        }
    }

    fn conclude(&self) {
        let found = self.0.fixture.found();
        let mut state = self.state();
        let outcome = if found.is_some() {
            CheckOutcome::Found
        } else {
            CheckOutcome::UpToDate
        };
        state.status.progress = found.as_ref().and(self.0.fixture.progress);
        state.status.ready = found.is_some() && self.0.fixture.ready;
        state.status.available = found;
        state.status.checking = false;
        state.status.error = None;
        state.status.last_check = Some(LastCheck {
            at_ms: now_ms(),
            outcome,
        });
    }

    #[cfg(test)]
    fn installs(&self) -> usize {
        self.state().installs
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

impl UpdatePort for FixturePort {
    fn attach(&self, notifier: UpdateNotifier) {
        self.state().notifier = Some(notifier);
    }

    fn status(&self) -> PortStatus {
        self.state().status.clone()
    }

    fn apply(&self, policy: &UpdatePolicy) {
        let launch = {
            let mut state = self.state();
            let launch = !state.launched
                && policy.checks
                && self.0.fixture.launch_check.unwrap_or(true)
                && state.status.updater.mechanism()
                    != luxforge_core::update::Mechanism::Unavailable;
            state.launched |= launch;
            launch
        };
        if launch {
            self.run_check(None);
        }
    }

    fn check(&self, done: CheckDone) {
        self.run_check(Some(done));
    }

    fn request_install(&self) -> Result<(), Error> {
        let mut state = self.state();
        if let Some(refusal) = state.refusal.take() {
            return Err(refusal);
        }
        state.installs += 1;
        Ok(())
    }

    fn skip(&self, _: &str) {}

    fn move_to_applications(&self, _replace: bool) -> Result<(), Error> {
        Err(Error::not_ready(
            "this installation cannot move into Applications",
        ))
    }
}

/// The host this launch runs with, its update port replaced by the evidence fixture's when it
/// names one.
pub(crate) fn installed(
    fixture: Option<&UpdateFixture>,
    host: luxforge_core::HostConfig,
) -> luxforge_core::HostConfig {
    match fixture {
        Some(fixture) => luxforge_core::HostConfig {
            updates: Some(Arc::new(FixturePort::new(fixture.clone()))),
            ..host
        },
        None => host,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_a_fixture_is_read_strictly() {
        let fixture = UpdateFixture::parse(
            r#"{"release": {"version": "0.2.0", "build": 42, "catalog_format": 99},
                "refuse_install": {"code": "conflict", "message": "busy",
                                   "data": {"jobs": [{"job_id": "j", "kind": "export"}]}}}"#,
        )
        .unwrap();
        assert_eq!(fixture.found().unwrap().catalog_format, 99);
        assert_eq!(fixture.updater().unwrap(), Updater::Sparkle);
        for bad in [
            r#"{"mechanism": "flatpak"}"#,
            r#"{"mechanism": "unavailable", "reason": "tuesday"}"#,
            r#"{"refuse_install": {"code": "forbidden", "message": "x"}}"#,
            r#"{"check_ms": 300}"#,
            r#"{"colour": "red"}"#,
        ] {
            assert!(UpdateFixture::parse(bad).is_err(), "{bad}");
        }
        let unavailable = UpdateFixture::parse(r#"{"mechanism": "unavailable"}"#).unwrap();
        assert_eq!(
            unavailable.updater().unwrap(),
            Updater::Unavailable(UnavailableReason::DevelopmentBuild)
        );
    }

    #[test]
    fn update_a_fixture_port_checks_at_launch_and_refuses_one_install() {
        let fixture = UpdateFixture::parse(
            r#"{"release": {"version": "0.2.0", "build": 42}, "ready": true,
                "refuse_install": {"code": "not-ready", "message": "later",
                                   "data": {"reason": "no-update"}}}"#,
        )
        .unwrap();
        let port = FixturePort::new(fixture);
        let (sender, receiver) = std::sync::mpsc::channel();
        port.attach(UpdateNotifier::new(move || {
            let _ = sender.send(());
        }));
        port.apply(&UpdatePolicy::new(true, false, None));
        assert!(receiver.try_recv().is_ok(), "the launch check signalled");
        let status = port.status();
        assert_eq!(status.available.unwrap().version, "0.2.0");
        assert!(status.ready && !status.checking);
        // The policy applied again checks nothing more at launch.
        port.apply(&UpdatePolicy::new(true, true, None));
        assert!(!port.status().checking);
        let refused = port.request_install().unwrap_err();
        assert_eq!(refused.kind, ErrorKind::NotReady);
        port.request_install().unwrap();
        assert_eq!(port.installs(), 1);
    }
}
