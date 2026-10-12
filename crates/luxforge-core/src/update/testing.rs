//! A fake [`UpdatePort`] for tests of the update commands and of what the desktop shows of them.
//! It touches no network, feed or file: a test scripts what a check finds, holds checks to act
//! while one runs, and reads back every call the core made. Built for the core's own tests and,
//! with the `test-holds` feature, for any crate's tests:
//!
//! ```ignore
//! let port = FakeUpdatePort::new(Updater::Sparkle);
//! port.finds(Some(FakeUpdatePort::release("0.2.0", 2)));
//! let host = HostConfig { updates: Some(port.clone()), ..HostConfig::unconfigured() };
//! // update.check answers with the release found; update.install reaches `port.installs()`.
//! ```
use super::{
    CATALOG_FORMAT, CheckDone, CheckOutcome, FeedUpdate, LastCheck, PortStatus, UpdateNotifier,
    UpdatePolicy, UpdatePort, Updater,
};
use crate::Error;
use std::sync::{Arc, Mutex, MutexGuard};

/// What the next check finds.
#[derive(Clone, Debug)]
enum Finding {
    Release(Option<FeedUpdate>),
    Failure(String),
}

struct State {
    status: PortStatus,
    notifier: Option<UpdateNotifier>,
    attached: usize,
    policies: Vec<UpdatePolicy>,
    finding: Finding,
    hold: bool,
    held: Vec<CheckDone>,
    checks: usize,
    installs: usize,
    /// Each move requested, by its `replace`.
    moves: Vec<bool>,
    skips: Vec<String>,
    refusal: Option<Error>,
    clock: i64,
}

/// A scripted update port: see the module's documentation.
pub struct FakeUpdatePort(Mutex<State>);

impl FakeUpdatePort {
    /// A port with `updater` that has checked nothing and finds nothing newer until told.
    pub fn new(updater: Updater) -> Arc<Self> {
        Arc::new(Self(Mutex::new(State {
            status: PortStatus {
                build: Some(1),
                ..PortStatus::idle(updater)
            },
            notifier: None,
            attached: 0,
            policies: Vec::new(),
            finding: Finding::Release(None),
            hold: false,
            held: Vec::new(),
            checks: 0,
            installs: 0,
            moves: Vec::new(),
            skips: Vec::new(),
            refusal: None,
            clock: 1_000,
        })))
    }

    /// A release on the default channel with this build's catalog format.
    pub fn release(version: &str, build: u64) -> FeedUpdate {
        FeedUpdate {
            version: version.into(),
            build,
            notes_url: Some(format!(
                "https://github.com/luxforge/luxforge/releases/tag/v{version}"
            )),
            catalog_format: CATALOG_FORMAT,
            prerelease: false,
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// What every check from now on finds: a newer release, or nothing.
    pub fn finds(&self, release: Option<FeedUpdate>) {
        self.state().finding = Finding::Release(release);
    }

    /// Every check from now on fails with `message`.
    pub fn fails(&self, message: &str) {
        self.state().finding = Finding::Failure(message.into());
    }

    /// Hold every check from now on, reporting it running, until [`Self::release_checks`]; or
    /// answer each at once again.
    pub fn hold_checks(&self, hold: bool) {
        self.state().hold = hold;
    }

    /// End every held check with what checks find now.
    pub fn release_checks(&self) {
        let held = {
            let mut state = self.state();
            let held = std::mem::take(&mut state.held);
            if !held.is_empty() {
                Self::conclude(&mut state);
            }
            held
        };
        // Finished outside the lock: finishing wakes the owner, which reads the status.
        for done in held {
            done.finish();
        }
    }

    /// Change what the port reports, as its mechanism would, and signal the owner.
    pub fn update(&self, change: impl FnOnce(&mut PortStatus)) {
        let notifier = {
            let mut state = self.state();
            change(&mut state.status);
            state.notifier.clone()
        };
        if let Some(notifier) = notifier {
            notifier.changed();
        }
    }

    /// Refuse the next install or move with `error`, as a port's own refusal.
    pub fn refuse_next(&self, error: Error) {
        self.state().refusal = Some(error);
    }

    /// How many times an owner attached.
    pub fn attached(&self) -> usize {
        self.state().attached
    }

    /// The latest policy applied.
    pub fn policy(&self) -> Option<UpdatePolicy> {
        self.state().policies.last().cloned()
    }

    /// How many policies were applied.
    pub fn applied(&self) -> usize {
        self.state().policies.len()
    }

    /// How many checks the core started.
    pub fn checks(&self) -> usize {
        self.state().checks
    }

    /// How many checks are held running.
    pub fn held_checks(&self) -> usize {
        self.state().held.len()
    }

    /// How many installs the core requested.
    pub fn installs(&self) -> usize {
        self.state().installs
    }

    /// How many moves into Applications the core requested.
    pub fn moves(&self) -> usize {
        self.state().moves.len()
    }

    /// Each move into Applications the core requested, by whether it may replace an existing app.
    pub fn move_requests(&self) -> Vec<bool> {
        self.state().moves.clone()
    }

    /// Every version skipped, in order.
    pub fn skips(&self) -> Vec<String> {
        self.state().skips.clone()
    }

    /// Record what a check found, as a finished check does.
    fn conclude(state: &mut State) {
        state.clock += 1_000;
        let outcome = match &state.finding {
            Finding::Release(release) => {
                state.status.available = release.clone();
                state.status.error = None;
                if release.is_some() {
                    CheckOutcome::Found
                } else {
                    CheckOutcome::UpToDate
                }
            }
            Finding::Failure(message) => {
                state.status.error = Some(message.clone());
                CheckOutcome::Failed
            }
        };
        state.status.checking = false;
        state.status.last_check = Some(LastCheck {
            at_ms: state.clock,
            outcome,
        });
    }
}

impl UpdatePort for FakeUpdatePort {
    fn attach(&self, notifier: UpdateNotifier) {
        let mut state = self.state();
        state.notifier = Some(notifier);
        state.attached += 1;
    }

    fn status(&self) -> PortStatus {
        self.state().status.clone()
    }

    fn apply(&self, policy: &UpdatePolicy) {
        self.state().policies.push(policy.clone());
    }

    fn check(&self, done: CheckDone) {
        let mut state = self.state();
        state.checks += 1;
        if state.hold {
            state.status.checking = true;
            state.held.push(done);
            return;
        }
        Self::conclude(&mut state);
        drop(state);
        done.finish();
    }

    fn request_install(&self) -> Result<(), Error> {
        let mut state = self.state();
        if let Some(error) = state.refusal.take() {
            return Err(error);
        }
        state.installs += 1;
        Ok(())
    }

    fn skip(&self, version: &str) {
        self.state().skips.push(version.into());
    }

    fn move_to_applications(&self, replace: bool) -> Result<(), Error> {
        let mut state = self.state();
        if let Some(error) = state.refusal.take() {
            return Err(error);
        }
        state.moves.push(replace);
        Ok(())
    }
}
