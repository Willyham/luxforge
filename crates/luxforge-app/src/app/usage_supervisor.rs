//! The desktop's one usage statistics supervisor
//! ([design](../../../../docs/design/usage-statistics.md#implemented-integration)).
//!
//! An eligible launch starts it once, in [`start`], before the catalog owner reads the stored
//! consent, and keeps it for the process's lifetime: it shares the host's collector, consent
//! runtime, preferences directory and network transport, gathers the device profile from what the
//! renderer machinery already knows ([`Observation`]), and samples the process through the core's
//! [`Sampler`]. An ineligible launch, such as every production build while the activation switch
//! is off, starts nothing, and so does `luxforge-json`, which never calls this.
//!
//! The update loop feeds it four facts, each cheap and none per frame: whether the window is
//! presented ([`Editor::usage_presented`], from the native visibility gate), each deliberate
//! interaction ([`interacted`], from the raw key, button and scroll events and from each evidence
//! step a script performs for a person), a change of renderer or adapter
//! ([`Editor::usage_observe`]) and the close ([`shutdown`]). Nothing here wakes the update loop:
//! the supervisor's worker never sends the GUI a message.
//!
//! The supervisor lives in a process-wide slot because the raw event filter that sees each key
//! press and click is a plain function with no access to the editor; the slot is filled at most
//! once per process, by [`start`], and unit tests, which never call it, leave it empty, so every
//! feed is a no-op there.
//!
//! An evidence launch with a test receiver (`--usage-test-receiver`) gets two test seams, never
//! available otherwise: the fixed release [`TEST_RELEASE`], since an unrevisioned build has none,
//! and an [`EvidenceClock`] whose monotonic time a script step can move forward
//! ([`advance`]), so a window falls due without waiting ten real minutes. Its jitter is zero.
use super::{Editor, usage_device::Observation};
use luxforge_core::{
    HostConfig,
    usage::{
        Clock, GatheredDevice, Randomness, Release, SessionClock, Supervisor, SupervisorConfig,
        sampler::Sampler,
    },
};
use std::{
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// The release an evidence launch with a test receiver reports, in place of the packaged build's
/// `<version>+<revision>`.
pub(crate) const TEST_RELEASE: &str = "0.0.0+000000e71de5";

/// The process's supervisor and what feeds it.
struct Process {
    supervisor: Supervisor,
    /// The renderer facts the device profile is gathered from: written on the update loop when
    /// they change, read on the statistics worker.
    observation: Arc<Mutex<Option<Observation>>>,
    /// The evidence clock, in a launch with a test receiver.
    clock: Option<Arc<EvidenceClock>>,
    /// The release batches report.
    release: Option<String>,
}

static PROCESS: OnceLock<Process> = OnceLock::new();

/// What a launch's log records of its statistics supervisor: never an identifier.
pub(crate) fn summary() -> serde_json::Value {
    match PROCESS.get() {
        Some(process) => serde_json::json!({
            "started": true,
            "release": process.release,
            "test_clock": process.clock.is_some(),
        }),
        None => serde_json::json!({"started": false}),
    }
}

/// Start the process's supervisor for an eligible launch, before the owner reads the stored
/// consent, so a stored opt-in starts its worker as soon as it is read. `test` says the launch
/// has a test receiver, which gets the evidence seams. `presented` is the window's state at launch.
/// Nothing is started for an ineligible launch.
pub(super) fn start(host: &HostConfig, test: bool, presented: bool) {
    if !host.usage_consent.eligibility().is_eligible() || PROCESS.get().is_some() {
        return;
    }
    let observation: Arc<Mutex<Option<Observation>>> = Arc::default();
    let observed = observation.clone();
    // Runs on the statistics worker: the window's adapter may be looked up there, once per
    // observation, and nothing waits on the update loop.
    let device = GatheredDevice::new(move || {
        let observation = observed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()?;
        let resolved = observation.resolve_on_worker().ok()?;
        Some((resolved.gpus, resolved.path))
    });
    let mut config = SupervisorConfig::new(
        host.usage_consent.clone(),
        host.usage.clone(),
        host.preferences_dir.clone(),
        host.transport.clone(),
        Box::new(device),
    );
    config.sampler = Box::new(Sampler::new());
    let clock = test.then(|| Arc::new(EvidenceClock::new()));
    if let Some(clock) = &clock {
        config.release = Release::parse(TEST_RELEASE);
        config.clock = clock.clone();
        config.randomness = Box::new(NoJitter);
    }
    let release = config
        .release
        .as_ref()
        .map(|release| release.as_str().to_owned());
    let supervisor = Supervisor::start(config);
    supervisor.set_presented(presented);
    let _ = PROCESS.set(Process {
        supervisor,
        observation,
        clock,
        release,
    });
}

/// A deliberate interaction. Called from the raw event filter for every key press, button press
/// and scroll, so it is a lock and a clock read at most.
pub(crate) fn interacted() {
    if let Some(process) = PROCESS.get() {
        process.supervisor.interacted();
    }
}

/// Quit: the supervisor cancels any request in flight and lets its worker end; nothing waits.
pub(super) fn shutdown() {
    if let Some(process) = PROCESS.get() {
        process.supervisor.shutdown();
    }
}

/// Move the evidence clock forward by `by` and wake the worker, so whatever falls due in that
/// time is done now. The move counts as an interaction, so it never makes the window idle. `false` when the launch has no evidence clock.
pub(crate) fn advance(by: Duration) -> bool {
    let Some((process, clock)) = PROCESS
        .get()
        .and_then(|process| Some((process, process.clock.as_ref()?)))
    else {
        return false;
    };
    clock.advance(by);
    // The script's own move of the clock is not a minute without interaction.
    process.supervisor.interacted();
    process.supervisor.resumed();
    true
}

impl Editor {
    /// Report the window's visibility once the native gate is ready, when it changed.
    pub(crate) fn usage_presented(&mut self) {
        if !self.visibility.ready {
            return;
        }
        let presented = self.visibility.sampling_allowed();
        if self.usage_reported_presented == Some(presented) {
            return;
        }
        self.usage_reported_presented = Some(presented);
        if let Some(process) = PROCESS.get() {
            process.supervisor.set_presented(presented);
        }
    }

    /// The renderer or the window's adapter changed: keep what the editor knows now for the
    /// statistics worker, and have it gather the profile again when that differs from before.
    pub(crate) fn usage_observe(&mut self) {
        let Some(process) = PROCESS.get() else {
            return;
        };
        let now = Observation::of(self);
        let mut held = process
            .observation
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if held.as_ref() == Some(&now) {
            return;
        }
        *held = Some(now);
        drop(held);
        process.supervisor.device_changed();
    }
}

/// The platform's clocks, with a monotonic offset an evidence script moves forward. Only the
/// worker's schedule sees it: a session's event times and the wall clock stay real, so the move is
/// never taken for a system sleep (the wall clock never gains on the monotonic one), and the
/// process sampler, whose rates come from its readings' own real times, takes no sample across
/// it rather than a fabricated one.
#[derive(Debug)]
pub(crate) struct EvidenceClock {
    origin: Instant,
    offset_ms: AtomicU64,
}

impl EvidenceClock {
    pub(crate) fn new() -> Self {
        Self {
            origin: Instant::now(),
            offset_ms: AtomicU64::new(0),
        }
    }

    pub(crate) fn advance(&self, by: Duration) {
        let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        self.offset_ms.fetch_add(millis, Ordering::AcqRel);
    }
}

impl Clock for EvidenceClock {
    fn monotonic(&self) -> Duration {
        self.origin.elapsed() + Duration::from_millis(self.offset_ms.load(Ordering::Acquire))
    }

    fn wall(&self) -> Duration {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
    }

    fn session_clock(&self) -> SessionClock {
        SessionClock::monotonic()
    }
}

/// No jitter, so an evidence window falls due exactly when its clock says.
struct NoJitter;

impl Randomness for NoJitter {
    fn up_to(&mut self, _: Duration) -> Duration {
        Duration::ZERO
    }
}
