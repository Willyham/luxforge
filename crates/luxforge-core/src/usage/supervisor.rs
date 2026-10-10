//! The process-wide statistics supervisor and its one bounded background sender
//! ([design](../../../../docs/design/usage-statistics.md#delivery-policy)).
//!
//! A [`Supervisor`] is created once per desktop process. It owns nothing until collection becomes
//! effective: no thread, timer, endpoint lookup or buffer exists before consent, in an ineligible
//! launch or after an opt-out. The consent runtime's watcher ([`Usage::watch`]) tells it of every
//! change, a coalesced wake rather than a poll. When consent is effective it starts one worker
//! thread, which starts the collector's session (trigger `launch` for a stored opt-in, `opt-in` for
//! one made now) and then sleeps until its next necessary deadline or a wake: a window becoming
//! nonempty or full, visibility, a device change, a consent change or quit. When consent ends the
//! worker stops the collector, frees every buffer and exits.
//!
//! Scheduling follows [`super::sender`]: a nonempty window is due ten minutes after its first event,
//! or as soon as it holds 512 events, plus 0–60 s of jitter chosen once per window; attempts are at
//! least 60 s apart and the first is at least 60 s after the session began; a window whose oldest
//! event is 30 minutes old is discarded unsent; there is no hourly or daily cap. One request is in
//! flight at most and nothing is queued: each detached window is attempted once and dropped. A
//! hidden window starts no upload and keeps no timer. Before detaching a window and again
//! immediately before the transport gets it, the worker checks eligibility, the generation and the
//! stored record ([`Usage::confirm_stored`]), so another process's opt-out is never followed by an
//! upload.
//!
//! An opt-out stops the collector and cancels the request in flight through its [`JobControl`],
//! which shuts the socket, from the thread that turned consent off, before the opt-out is written.
//! A request's outcome that arrives after its generation ended changes nothing. Quitting cancels and
//! discards without joining the worker. Nothing here wakes the GUI: delivery state is only the
//! bounded counts [`Usage::set_diagnostics`] reports through `usage.read`.
//!
//! The host reports its window's visibility ([`Supervisor::set_presented`]) and each deliberate
//! interaction ([`Supervisor::interacted`]). After the first report, a change of visibility records
//! `app.presented`; a presented window with no interaction for [`IDLE_AFTER`] is idle, which the
//! worker notices at its own deadline and records as `app.idle` (an interaction records its end),
//! and the sampler pauses while it lasts. Neither adds a timer of its own beyond that one deadline.
//!
//! The monotonic clock stops while macOS sleeps, so the worker compares it with the wall clock at
//! each wake and starts a new session after a sleep longer than [`SLEEP_GAP`], discarding the
//! window recorded before it. A device profile change discards the window recorded under the old
//! profile without forcing an early request.
use super::{
    Active, Collector, ConsentReader, DeviceProfile, Diagnostics, EventKind, Gate, Generation,
    Idle, Presented, Release, ReportId, Session, SessionClock, SessionStart, Trigger, Usage,
    device,
    encode::BodyBuffer,
    sampler,
    sender::{self, Cooldown, Delivery, Suspension, Verdict},
};
use crate::{
    capabilities::transport::{RedirectPolicy, SendOptions, Transport, TransportRequest},
    jobs::JobControl,
};
use std::{
    fmt, io,
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, Weak,
        atomic::{AtomicU32, Ordering},
    },
    thread::{self, Thread},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// The wall clock running this much further than the monotonic clock between two wakes is a
/// system sleep, which ends the session.
pub const SLEEP_GAP: Duration = Duration::from_secs(10);
/// The shortest wait the worker schedules, so a deadline that has passed without anything to do
/// cannot spin it.
const MIN_WAIT: Duration = Duration::from_secs(1);
/// A presented window with no deliberate interaction for this long is idle (`app.idle`).
pub const IDLE_AFTER: Duration = Duration::from_secs(60);
/// The reason a request is cancelled with when consent ends.
const REVOKED: &str = "usage statistics were turned off";
/// The reason a request is cancelled with when the application quits.
const QUITTING: &str = "the application is quitting";

/// The clocks the supervisor schedules by, and how its worker waits.
pub trait Clock: Send + Sync + fmt::Debug {
    /// Time on a monotonic clock since an arbitrary origin. It may stop while the system sleeps,
    /// as macOS's does, which the supervisor detects against [`Clock::wall`].
    fn monotonic(&self) -> Duration;
    /// UTC wall-clock time since the Unix epoch, which runs on through a sleep.
    fn wall(&self) -> Duration;
    /// The clock for a new session's event times, starting now and running with
    /// [`Clock::monotonic`].
    fn session_clock(&self) -> SessionClock;
    /// Block the calling worker until `deadline` on the monotonic clock (`None`: indefinitely) or
    /// until its thread is unparked. May return early.
    fn wait(&self, deadline: Option<Duration>) {
        match deadline {
            None => thread::park(),
            Some(deadline) => {
                if let Some(rest) = deadline.checked_sub(self.monotonic()) {
                    thread::park_timeout(rest);
                }
            }
        }
    }
}

/// The platform's clocks: [`Instant`] and [`SystemTime`].
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn monotonic(&self) -> Duration {
        self.origin.elapsed()
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

/// Clocks a test or simulation moves: [`ManualClock::advance`] moves both and wakes a waiting
/// worker, [`ManualClock::suspend`] moves only the wall clock, as a system sleep does.
#[derive(Debug, Default)]
pub struct ManualClock {
    state: Mutex<ManualState>,
}

#[derive(Debug, Default)]
struct ManualState {
    monotonic: Duration,
    wall: Duration,
    /// Each session clock handed out, with the monotonic time it started at.
    sessions: Vec<(Arc<AtomicU32>, Duration)>,
    waiters: Vec<Thread>,
}

impl ManualClock {
    /// Monotonic time zero at wall-clock time `wall` since the Unix epoch.
    pub fn new(wall: Duration) -> Self {
        Self {
            state: Mutex::new(ManualState {
                wall,
                ..ManualState::default()
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, ManualState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Let `by` pass on both clocks, and wake every worker waiting on this clock.
    pub fn advance(&self, by: Duration) {
        let mut state = self.lock();
        state.monotonic += by;
        state.wall += by;
        let now = state.monotonic;
        for (millis, started) in &state.sessions {
            let elapsed = now.saturating_sub(*started).as_millis();
            millis.store(
                u32::try_from(elapsed).unwrap_or(u32::MAX),
                Ordering::Release,
            );
        }
        for waiter in state.waiters.drain(..) {
            waiter.unpark();
        }
    }

    /// A system sleep of `by`: the wall clock moves, the monotonic clock does not, and nothing
    /// wakes.
    pub fn suspend(&self, by: Duration) {
        self.lock().wall += by;
    }
}

impl Clock for ManualClock {
    fn monotonic(&self) -> Duration {
        self.lock().monotonic
    }

    fn wall(&self) -> Duration {
        self.lock().wall
    }

    fn session_clock(&self) -> SessionClock {
        let (clock, millis) = SessionClock::manual();
        let mut state = self.lock();
        state
            .sessions
            .retain(|(handle, _)| Arc::strong_count(handle) > 1);
        let now = state.monotonic;
        state.sessions.push((millis, now));
        clock
    }

    fn wait(&self, deadline: Option<Duration>) {
        {
            let mut state = self.lock();
            if deadline.is_some_and(|deadline| deadline <= state.monotonic) {
                return;
            }
            let me = thread::current();
            if !state.waiters.iter().any(|waiter| waiter.id() == me.id()) {
                state.waiters.push(me);
            }
        }
        thread::park();
    }
}

/// Where the worker's jitter comes from.
pub trait Randomness: Send {
    /// A uniformly distributed duration in `0..=max`, to the millisecond.
    fn up_to(&mut self, max: Duration) -> Duration;
}

/// SplitMix64 seeded from the system's randomness: jitter needs no cryptographic strength.
#[derive(Debug)]
pub struct SystemRandomness(u64);

impl SystemRandomness {
    pub fn new() -> Self {
        let mut seed = [0; 8];
        if getrandom::fill(&mut seed).is_err() {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            seed = (nanos as u64).to_le_bytes();
        }
        Self(u64::from_le_bytes(seed))
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
}

impl Default for SystemRandomness {
    fn default() -> Self {
        Self::new()
    }
}

impl Randomness for SystemRandomness {
    fn up_to(&mut self, max: Duration) -> Duration {
        let millis = u64::try_from(max.as_millis()).unwrap_or(u64::MAX);
        if millis == 0 {
            return Duration::ZERO;
        }
        Duration::from_millis(self.next() % millis.saturating_add(1))
    }
}

/// The process performance sampler the worker drives, in [`sampler::Sampler`]'s shape. The worker
/// calls it only between its own waits, never on the UI or an owner thread.
pub trait Sampling: Send {
    /// The window became visible or was hidden or minimized; a no-op when unchanged.
    fn set_presented(&mut self, presented: bool);
    /// The person went idle or became active again; a no-op when unchanged.
    fn set_idle(&mut self, idle: bool);
    /// The device profile changed: the window being measured belongs to the old one.
    fn profile_changed(&mut self);
    /// Do whatever is due at `monotonic` (on the supervisor's clock) and `wall`, recording into
    /// `collector`'s current session, and say when to call again; `None` for no deadline.
    fn step(
        &mut self,
        collector: &Collector,
        monotonic: Duration,
        wall: Duration,
    ) -> Option<Duration>;
}

impl<S: sampler::Source + Send> Sampling for sampler::Sampler<S> {
    fn set_presented(&mut self, presented: bool) {
        sampler::Sampler::set_presented(self, presented);
    }

    fn set_idle(&mut self, idle: bool) {
        sampler::Sampler::set_idle(self, idle);
    }

    fn profile_changed(&mut self) {
        sampler::Sampler::profile_changed(self);
    }

    fn step(
        &mut self,
        collector: &Collector,
        monotonic: Duration,
        wall: Duration,
    ) -> Option<Duration> {
        match sampler::Sampler::step(self, collector, sampler::Stamp { monotonic, wall }) {
            sampler::Next::At(due) => Some(due),
            sampler::Next::Parked => None,
        }
    }
}

/// No process sampling.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoSampling;

impl Sampling for NoSampling {
    fn set_presented(&mut self, _: bool) {}

    fn set_idle(&mut self, _: bool) {}

    fn profile_changed(&mut self) {}

    fn step(&mut self, _: &Collector, _: Duration, _: Duration) -> Option<Duration> {
        None
    }
}

/// Why a [`DeviceSource`] has no profile now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceUnavailable {
    /// Not known yet (the renderer's adapter facts are pending): asked again at the next attempt,
    /// and a window due meanwhile is discarded unsent.
    Pending,
    /// Collection ended, or this is no longer the generation it was asked for.
    NotConsented,
    /// No reportable device for this launch ([`device::GatherError::Missing`] or
    /// [`device::GatherError::Invalid`]): the collector stops for this consent generation and
    /// nothing is gathered again until the next opt-in or launch.
    Unreportable,
}

/// The device profile, gathered on the worker: when a session starts, after
/// [`Supervisor::device_changed`] (`refresh`), and at an attempt while none is known. May block.
pub trait DeviceSource: Send {
    /// The profile for `gate`'s (the collector's) current generation.
    fn profile(&mut self, gate: &Gate, refresh: bool) -> Result<DeviceProfile, DeviceUnavailable>;
    /// Collection ended: forget the profile.
    fn clear(&mut self);
}

/// The production [`DeviceSource`]: a [`device::DeviceContext`] over the adapters and renderer path
/// the host reports through `observe`, which answers `None` while they are not known yet.
pub struct GatheredDevice<F> {
    context: device::DeviceContext,
    observe: F,
}

impl<F> GatheredDevice<F>
where
    F: FnMut() -> Option<(Vec<device::GpuFacts>, device::RendererPath)> + Send,
{
    pub fn new(observe: F) -> Self {
        Self {
            context: device::DeviceContext::new(),
            observe,
        }
    }
}

impl<F> DeviceSource for GatheredDevice<F>
where
    F: FnMut() -> Option<(Vec<device::GpuFacts>, device::RendererPath)> + Send,
{
    fn profile(&mut self, gate: &Gate, refresh: bool) -> Result<DeviceProfile, DeviceUnavailable> {
        if refresh || self.context.profile(gate).is_none() {
            match (self.observe)() {
                Some((gpus, path)) => match self.context.update(gate, &gpus, path) {
                    Ok(_) => {}
                    Err(device::GatherError::NotConsented) => {
                        return Err(DeviceUnavailable::NotConsented);
                    }
                    Err(device::GatherError::Missing(_) | device::GatherError::Invalid(_)) => {
                        return Err(DeviceUnavailable::Unreportable);
                    }
                },
                // Facts not known yet: keep what is held, if anything.
                None if self.context.profile(gate).is_none() => {
                    return Err(DeviceUnavailable::Pending);
                }
                None => {}
            }
        }
        self.context
            .profile(gate)
            .copied()
            .ok_or(DeviceUnavailable::NotConsented)
    }

    fn clear(&mut self) {
        self.context.clear();
    }
}

/// A [`DeviceSource`] answering from a function, for hosts and tests that already hold a profile.
pub struct DeviceFn<F>(pub F);

impl<F> DeviceSource for DeviceFn<F>
where
    F: FnMut() -> Result<DeviceProfile, DeviceUnavailable> + Send,
{
    fn profile(&mut self, _: &Gate, _: bool) -> Result<DeviceProfile, DeviceUnavailable> {
        (self.0)()
    }

    fn clear(&mut self) {}
}

/// What a supervisor is made of. [`Config::new`] fills in the platform clocks, system jitter, the
/// build's release and no sampler.
pub struct Config {
    pub usage: Arc<Usage>,
    pub collector: Arc<Collector>,
    /// The directory of `preferences.json`, whose stored consent the worker rereads.
    pub preferences_dir: Option<PathBuf>,
    pub transport: Arc<dyn Transport>,
    /// The release batches report; without one nothing is collected or sent.
    pub release: Option<Release>,
    pub device: Box<dyn DeviceSource>,
    pub sampler: Box<dyn Sampling>,
    pub clock: Arc<dyn Clock>,
    pub randomness: Box<dyn Randomness>,
}

impl Config {
    pub fn new(
        usage: Arc<Usage>,
        collector: Arc<Collector>,
        preferences_dir: Option<PathBuf>,
        transport: Arc<dyn Transport>,
        device: Box<dyn DeviceSource>,
    ) -> Self {
        let release = usage.release();
        Self {
            usage,
            collector,
            preferences_dir,
            transport,
            release,
            device,
            sampler: Box::new(NoSampling),
            clock: Arc::new(SystemClock::new()),
            randomness: Box::new(SystemRandomness::new()),
        }
    }
}

/// The process's statistics supervisor. Dropping it quits: see [`Supervisor::shutdown`].
pub struct Supervisor {
    shared: Arc<Shared>,
}

impl fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Supervisor")
            .field("running", &self.running())
            .finish_non_exhaustive()
    }
}

impl Supervisor {
    /// Register with the consent runtime and, if collection is already effective, start the worker.
    /// The window starts as not presented: the desktop reports visibility with
    /// [`Supervisor::set_presented`].
    pub fn start(config: Config) -> Self {
        let shared = Shared::new(config, true);
        Shared::watch(&shared);
        Self { shared }
    }

    /// Whether the window can be seen (neither minimized nor hidden). A hidden window starts no
    /// upload and keeps no timer. The first report is the window's state at launch; each later
    /// change records `app.presented` in the current session, if there is one. Reporting also
    /// starts idle tracking ([`Supervisor::interacted`]): a host that draws no window has neither.
    pub fn set_presented(&self, presented: bool) {
        self.shared.set_presented(presented);
    }

    /// A deliberate interaction: a key press, a click, a scroll or an operation a script performs
    /// for a person. Cheap enough for every input event: a lock and a clock read, and a wake only
    /// when it ends an idle period, which records `app.idle` `active`. The worker decides idleness
    /// itself, at its own deadline a minute after the last interaction while the window is
    /// presented; no timer polls for it.
    pub fn interacted(&self) {
        self.shared.interacted();
    }

    /// The renderer's adapter or path changed: the worker gathers the device profile again and
    /// discards the window recorded under the old one.
    pub fn device_changed(&self) {
        self.shared.signal(|state| state.device_changed = true);
    }

    /// The system woke from sleep: the worker checks for the gap at once rather than at its next
    /// wake.
    pub fn resumed(&self) {
        self.shared.signal(|_| {});
    }

    /// Quit: stop collecting, cancel the request in flight and let the worker end on its own. Never
    /// waits for the worker or the network.
    pub fn shutdown(&self) {
        self.shared.shutdown();
    }

    /// Whether a worker thread exists now.
    pub fn running(&self) -> bool {
        self.shared.lock().worker.is_some()
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// What the supervisor, the consent watcher and the worker share.
struct Shared {
    usage: Arc<Usage>,
    collector: Arc<Collector>,
    clock: Arc<dyn Clock>,
    /// Whether consent starts a worker thread; a test that steps the machine itself says no.
    spawn: bool,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    quit: bool,
    presented: bool,
    /// The host has reported its window's visibility: later changes are recorded, and idleness is
    /// tracked.
    windowed: bool,
    /// No deliberate interaction for [`IDLE_AFTER`] while presented; the sampler pauses.
    idle: bool,
    /// The last deliberate interaction, on the supervisor's monotonic clock.
    last_interaction: Duration,
    device_changed: bool,
    /// The running worker's thread, to unpark.
    worker: Option<Thread>,
    /// The machine while no worker runs; it keeps the launch's delivery state across workers.
    machine: Option<Machine>,
    /// The request handed to the transport, with the generation it belongs to.
    in_flight: Option<(Generation, Arc<JobControl>)>,
}

/// What one step reads of the shared state.
struct Signals {
    quit: bool,
    presented: bool,
    idle: bool,
    /// When the window becomes idle without further interaction, while idleness is tracked.
    idle_due: Option<Duration>,
    device_changed: bool,
    worker: Option<Thread>,
}

impl Shared {
    fn new(config: Config, spawn: bool) -> Arc<Self> {
        let Config {
            usage,
            collector,
            preferences_dir,
            transport,
            release,
            device,
            sampler,
            clock,
            randomness,
        } = config;
        usage.set_release(release);
        let machine = match release {
            Some(release) => Some(Machine::new(
                ConsentReader::new(preferences_dir),
                transport,
                release,
                clock.clone(),
                randomness,
                device,
                sampler,
            )),
            None => {
                usage.set_diagnostics(Diagnostics {
                    delivery: Delivery::Suspended(Suspension::Build),
                    ..Diagnostics::default()
                });
                None
            }
        };
        Arc::new(Self {
            usage,
            collector,
            clock,
            spawn,
            state: Mutex::new(State {
                machine,
                ..State::default()
            }),
        })
    }

    /// Register the consent watcher and act on the state consent is in now.
    fn watch(shared: &Arc<Self>) {
        if shared.lock().machine.is_none() {
            return;
        }
        let weak: Weak<Self> = Arc::downgrade(shared);
        shared.usage.watch(Some(Arc::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared.consent_changed();
            }
        })));
        shared.consent_changed();
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether consent is effective in an eligible launch.
    fn wanted(&self) -> bool {
        self.usage.eligibility().is_eligible() && self.usage.active().is_some()
    }

    /// The watcher: called on whichever thread changed consent, after the change. Revocation stops
    /// producers and cancels the request in flight here, before the opt-out is written; anything
    /// else only wakes the worker, or starts it.
    fn consent_changed(self: &Arc<Self>) {
        let current = self
            .usage
            .active()
            .filter(|_| self.usage.eligibility().is_eligible())
            .map(|active| active.generation);
        if current.is_none() {
            self.collector.stop();
        }
        let mut state = self.lock();
        if let Some((generation, job)) = &state.in_flight
            && current != Some(*generation)
        {
            job.cancel(REVOKED);
        }
        if let Some(worker) = &state.worker {
            worker.unpark();
        } else if current.is_some() && !state.quit && self.spawn && state.machine.is_some() {
            let shared = self.clone();
            if let Ok(worker) = thread::Builder::new()
                .name("luxforge-usage".into())
                .spawn(move || run(shared))
            {
                state.worker = Some(worker.thread().clone());
            }
        }
    }

    fn signal(&self, change: impl FnOnce(&mut State)) {
        let mut state = self.lock();
        change(&mut state);
        if let Some(worker) = &state.worker {
            worker.unpark();
        }
    }

    fn signals(&self) -> Signals {
        let mut state = self.lock();
        Signals {
            quit: state.quit,
            presented: state.presented,
            idle: state.idle,
            idle_due: (state.windowed && !state.idle).then(|| state.last_interaction + IDLE_AFTER),
            device_changed: std::mem::take(&mut state.device_changed),
            worker: state.worker.clone(),
        }
    }

    fn quitting(&self) -> bool {
        self.lock().quit
    }

    /// [`Supervisor::set_presented`].
    fn set_presented(&self, presented: bool) {
        self.signal(|state| {
            if state.windowed && state.presented != presented {
                self.collector.record(EventKind::AppPresented {
                    state: if presented {
                        Presented::Visible
                    } else {
                        Presented::Hidden
                    },
                });
            }
            if !state.windowed {
                state.windowed = true;
                state.last_interaction = self.clock.monotonic();
            }
            state.presented = presented;
        });
    }

    /// [`Supervisor::interacted`].
    fn interacted(&self) {
        let mut state = self.lock();
        if !state.windowed {
            return;
        }
        state.last_interaction = self.clock.monotonic();
        if state.idle {
            state.idle = false;
            self.collector.record(EventKind::AppIdle {
                state: Idle::Active,
            });
            if let Some(worker) = &state.worker {
                worker.unpark();
            }
        }
    }

    /// The worker's idle deadline passed: unless an interaction came meanwhile, the window is idle
    /// from now, recorded as `app.idle` `idle`. Answers whether it is idle.
    fn go_idle(&self, now: Duration) -> bool {
        let mut state = self.lock();
        if state.windowed
            && !state.idle
            && state.presented
            && now >= state.last_interaction + IDLE_AFTER
        {
            state.idle = true;
            self.collector
                .record(EventKind::AppIdle { state: Idle::Idle });
        }
        state.idle
    }

    fn publish(&self, generation: Generation, job: Arc<JobControl>) {
        self.lock().in_flight = Some((generation, job));
    }

    fn unpublish(&self) {
        self.lock().in_flight = None;
    }

    fn shutdown(&self) {
        self.usage.watch(None);
        {
            let mut state = self.lock();
            state.quit = true;
            if let Some((_, job)) = &state.in_flight {
                job.cancel(QUITTING);
            }
            if let Some(worker) = &state.worker {
                worker.unpark();
            }
        }
        self.collector.stop();
    }
}

/// The worker thread: step the machine, wait, and end when consent has ended or the application
/// quits, handing the machine back for a later opt-in.
fn run(shared: Arc<Shared>) {
    let Some(mut machine) = shared.lock().machine.take() else {
        shared.lock().worker = None;
        return;
    };
    loop {
        match machine.step(&shared) {
            Step::Wait(deadline) => shared.clock.wait(deadline),
            Step::Off => {
                let mut state = shared.lock();
                if state.quit || !shared.wanted() || machine.dormant(&shared) {
                    state.worker = None;
                    state.machine = Some(machine);
                    return;
                }
            }
        }
    }
}

/// What the worker does after one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    /// Wait until this monotonic deadline, or indefinitely, unless woken.
    Wait(Option<Duration>),
    /// Collection is not effective, has no reportable device in this generation, or the
    /// application quits: the worker may end.
    Off,
}

/// The one request's storage, reserved at the first attempt of a consent period and freed when it
/// ends.
struct Buffers {
    body: BodyBuffer,
    request: TransportRequest,
}

/// The current session and its window's schedule.
struct Live {
    session: Arc<Session>,
    consent: Generation,
    installation_id: ReportId,
    /// When the session began, on the monotonic clock.
    started: Duration,
    /// When the active window's first event was recorded, on the monotonic clock.
    first: Option<Duration>,
    /// The active window's jitter, chosen once.
    jitter: Option<Duration>,
    /// When the active window was first seen full.
    full_at: Option<Duration>,
}

/// The sender's state machine. One worker owns it at a time; tests step it directly.
struct Machine {
    reader: ConsentReader,
    transport: Arc<dyn Transport>,
    release: Release,
    clock: Arc<dyn Clock>,
    randomness: Box<dyn Randomness>,
    device: Box<dyn DeviceSource>,
    sampler: Box<dyn Sampling>,
    /// The profile the active window is recorded under.
    profile: Option<DeviceProfile>,
    /// The consent generation that has no reportable device: nothing is collected in it.
    no_device: Option<Generation>,
    /// The sampler's next deadline.
    sample_due: Option<Duration>,
    /// When the window becomes idle, unless someone interacts first.
    idle_due: Option<Duration>,
    buffers: Option<Buffers>,
    live: Option<Live>,
    presented: bool,
    /// The monotonic and wall times at the last step, to detect a sleep.
    observed: Option<(Duration, Duration)>,
    /// When the last attempt began.
    last_attempt: Option<Duration>,
    /// No attempt before this, after a failure or a `429`.
    not_before: Option<Duration>,
    cooldown: Option<Cooldown>,
    /// Consecutive failures, which choose the next cooldown; a success resets them.
    failures: u32,
    /// Sending is suspended until the next launch.
    suspended: Option<Suspension>,
    /// Counts of ended sessions, discarded windows, attempts and acceptances.
    totals: Diagnostics,
    /// Runs after a batch is serialized and before the checks that precede its handoff.
    #[cfg(test)]
    before_handoff: Option<Box<dyn FnMut() + Send>>,
}

impl Machine {
    fn new(
        reader: ConsentReader,
        transport: Arc<dyn Transport>,
        release: Release,
        clock: Arc<dyn Clock>,
        randomness: Box<dyn Randomness>,
        device: Box<dyn DeviceSource>,
        sampler: Box<dyn Sampling>,
    ) -> Self {
        Self {
            reader,
            transport,
            release,
            clock,
            randomness,
            device,
            sampler,
            profile: None,
            no_device: None,
            sample_due: None,
            idle_due: None,
            buffers: None,
            live: None,
            presented: false,
            observed: None,
            last_attempt: None,
            not_before: None,
            cooldown: None,
            failures: 0,
            suspended: None,
            totals: Diagnostics::default(),
            #[cfg(test)]
            before_handoff: None,
        }
    }

    /// One step: follow consent, sleep, visibility and device changes, sample, expire or send the
    /// window, publish the diagnostics and say how long to wait.
    fn step(&mut self, shared: &Shared) -> Step {
        let signals = shared.signals();
        if signals.quit {
            self.stop(shared);
            return Step::Off;
        }
        let now = self.clock.monotonic();
        let wall = self.clock.wall();
        let slept = self.observe(now, wall);
        let Some(active) = shared
            .usage
            .active()
            .filter(|_| shared.usage.eligibility().is_eligible())
        else {
            self.stop(shared);
            return Step::Off;
        };
        if self
            .live
            .as_ref()
            .is_some_and(|live| live.consent != active.generation)
        {
            self.stop(shared);
        }
        if self.no_device == Some(active.generation) {
            return Step::Off;
        }
        self.presented = signals.presented;
        self.sampler.set_presented(signals.presented);
        if self.live.is_none() {
            if !self.begin(shared, &active, active.trigger, now, wall, &signals.worker) {
                return self.after_begin_failed(shared, &active);
            }
        } else if slept {
            self.restart(shared, &active, now, wall, &signals.worker);
        }
        // Idleness is decided here, in the session it belongs to, at the deadline below.
        let idle = match signals.idle_due {
            Some(due) if signals.presented && now >= due => shared.go_idle(now),
            _ => signals.idle,
        };
        self.idle_due = signals.idle_due.filter(|_| !idle);
        self.sampler.set_idle(idle);
        if signals.device_changed && !self.gather(shared, true) {
            return Step::Off;
        }
        self.sample_due = self.live.as_ref().and_then(|_| {
            self.sampler
                .step(&shared.collector, self.clock.monotonic(), self.clock.wall())
        });
        self.schedule(shared, now);
        self.publish(shared, self.clock.monotonic());
        // The shortest wait counts from this step's start, so a deadline that passed while the
        // step ran is not pushed past it.
        Step::Wait(self.deadline(now))
    }

    /// Note the clocks; `true` when the wall clock ran ahead of the monotonic clock by more than
    /// [`SLEEP_GAP`] since the last step.
    fn observe(&mut self, now: Duration, wall: Duration) -> bool {
        let slept = self.observed.is_some_and(|(then, then_wall)| {
            let monotonic = now.saturating_sub(then);
            wall.saturating_sub(then_wall)
                .checked_sub(monotonic)
                .is_some_and(|gap| gap > SLEEP_GAP)
        });
        self.observed = Some((now, wall));
        slept
    }

    /// What a step answers when no session could start: `Off` when there is no reportable device
    /// in this generation, otherwise a wait for the next wake.
    fn after_begin_failed(&mut self, shared: &Shared, active: &Active) -> Step {
        if self.no_device == Some(active.generation) {
            let now = self.clock.monotonic();
            self.publish(shared, now);
            Step::Off
        } else {
            Step::Wait(None)
        }
    }

    /// Whether the worker may end because consent's current generation has no reportable device.
    fn dormant(&self, shared: &Shared) -> bool {
        self.no_device.is_some()
            && shared.usage.active().map(|active| active.generation) == self.no_device
    }

    /// Ask the device source for the profile; `refresh` after a device change. A changed profile
    /// discards the window recorded under the old one and resets the sampler's window. `false`
    /// when there is no reportable device: collection stops for this consent generation.
    fn gather(&mut self, shared: &Shared, refresh: bool) -> bool {
        match self.device.profile(shared.collector.gate(), refresh) {
            Ok(fresh) => {
                if self.profile.is_some_and(|held| held != fresh) {
                    self.discard_window();
                    self.sampler.profile_changed();
                }
                self.profile = Some(fresh);
                true
            }
            Err(DeviceUnavailable::Pending | DeviceUnavailable::NotConsented) => true,
            Err(DeviceUnavailable::Unreportable) => {
                let generation = self.live.as_ref().map(|live| live.consent);
                self.stop(shared);
                self.no_device = generation;
                let now = self.clock.monotonic();
                self.publish(shared, now);
                false
            }
        }
    }

    /// Start a session for `active`. `false` when the wall clock is outside the wire's years,
    /// consent ended meanwhile, or there is no reportable device.
    fn begin(
        &mut self,
        shared: &Shared,
        active: &Active,
        trigger: Trigger,
        now: Duration,
        wall: Duration,
        worker: &Option<Thread>,
    ) -> bool {
        let Some(started) = SessionStart::from_unix_seconds(wall.as_secs()) else {
            return false;
        };
        let session = start_collector(
            &shared.collector,
            active,
            trigger,
            started,
            self.clock.session_clock(),
        );
        // An opt-out that came between reading consent and starting the collector wins.
        if !shared.usage.gate().is_current(active.generation) {
            shared.collector.stop();
            return false;
        }
        if let Some(worker) = worker {
            session.set_waker(worker.clone());
        }
        shared.usage.set_session(Some(session.id()));
        self.live = Some(Live {
            session,
            consent: active.generation,
            installation_id: active.installation_id.report_id(),
            started: now,
            first: None,
            jitter: None,
            full_at: None,
        });
        if self
            .no_device
            .is_some_and(|generation| generation != active.generation)
        {
            self.no_device = None;
        }
        if self.profile.is_none() {
            return self.gather(shared, false);
        }
        true
    }

    /// After a sleep: discard the window, end the session and start the next one in the same
    /// consent generation.
    fn restart(
        &mut self,
        shared: &Shared,
        active: &Active,
        now: Duration,
        wall: Duration,
        worker: &Option<Thread>,
    ) {
        self.discard_window();
        if let Some(live) = self.live.take() {
            self.fold(&live.session);
        }
        shared.usage.set_session(None);
        self.begin(shared, active, Trigger::Launch, now, wall, worker);
    }

    /// Consent ended or the application quits: stop the collector and free every buffer.
    fn stop(&mut self, shared: &Shared) {
        shared.collector.stop();
        if let Some(live) = self.live.take() {
            self.fold(&live.session);
        }
        shared.usage.set_session(None);
        self.buffers = None;
        self.profile = None;
        self.device.clear();
        self.observed = None;
        self.sample_due = None;
        self.idle_due = None;
        let now = self.clock.monotonic();
        self.publish(shared, now);
    }

    /// Add an ending session's counts to the totals; its unsent window is dropped.
    fn fold(&mut self, session: &Session) {
        let counts = session.diagnostics();
        self.totals.recorded += counts.recorded;
        self.totals.dropped += counts.dropped + session.window().len as u64;
        self.totals.sent += counts.sent;
    }

    /// Detach the active window and drop it unsent: a `batch_seq` gap.
    fn discard_window(&mut self) {
        let Some(live) = &mut self.live else {
            return;
        };
        if let Some(detached) = live.session.detach() {
            self.totals.dropped += detached.events().len() as u64;
        }
        live.first = None;
        live.jitter = None;
        live.full_at = None;
    }

    /// Follow the active window: expire it, schedule it, and send it when it is due.
    fn schedule(&mut self, shared: &Shared, now: Duration) {
        let Some(live) = &mut self.live else {
            return;
        };
        let window = live.session.window();
        let Some(first_t) = window.first_t else {
            live.first = None;
            live.jitter = None;
            live.full_at = None;
            return;
        };
        let first = live.started + Duration::from_millis(u64::from(first_t));
        if now >= first + sender::WINDOW_AGE {
            self.discard_window();
            return;
        }
        live.first = Some(first);
        if window.len >= sender::WINDOW_EVENTS && live.full_at.is_none() {
            live.full_at = Some(now);
        }
        if live.jitter.is_none() {
            live.jitter = Some(self.randomness.up_to(sender::FLUSH_JITTER));
        }
        if self.presented
            && self.suspended.is_none()
            && self.earliest().is_some_and(|earliest| now >= earliest)
        {
            self.attempt(shared, now);
        }
    }

    /// When the active window may be sent: its due time, no sooner than a minute after the last
    /// attempt or the session's start, and not during a cooldown.
    fn earliest(&self) -> Option<Duration> {
        let live = self.live.as_ref()?;
        let first = live.first?;
        let mut due = first + sender::FLUSH_AFTER;
        if let Some(full) = live.full_at {
            due = due.min(full);
        }
        let mut earliest = due + live.jitter.unwrap_or_default();
        earliest = earliest.max(live.started + sender::ATTEMPT_INTERVAL);
        if let Some(last) = self.last_attempt {
            earliest = earliest.max(last + sender::ATTEMPT_INTERVAL);
        }
        if let Some(not_before) = self.not_before {
            earliest = earliest.max(not_before);
        }
        Some(earliest)
    }

    /// The next deadline: none while hidden; otherwise the window's send or expiry time, the
    /// sampler's and the moment the window becomes idle.
    fn deadline(&self, now: Duration) -> Option<Duration> {
        if !self.presented {
            return None;
        }
        let window = self.live.as_ref().and_then(|live| live.first);
        let send = self.earliest().filter(|_| self.suspended.is_none());
        let expiry = window.map(|first| first + sender::WINDOW_AGE);
        let sample = self.live.as_ref().and(self.sample_due);
        let idle = self.live.as_ref().and(self.idle_due);
        [send, expiry, sample, idle]
            .into_iter()
            .flatten()
            .min()
            .map(|deadline| deadline.max(now + MIN_WAIT))
    }

    /// The one attempt of the active window.
    fn attempt(&mut self, shared: &Shared, now: Duration) {
        let Some(live) = &self.live else {
            return;
        };
        let generation = live.consent;
        let installation_id = live.installation_id;
        let session = live.session.clone();
        if !shared.usage.eligibility().is_eligible()
            || !shared.usage.confirm_stored(&self.reader, generation)
        {
            self.stop(shared);
            return;
        }
        if self.profile.is_none() && !self.gather(shared, false) {
            return;
        }
        let Some(profile) = self.profile else {
            self.discard_window();
            return;
        };
        let mut buffers = match self.buffers.take() {
            Some(buffers) => buffers,
            None => match sender::request(shared.usage.eligibility()) {
                Ok(Some(request)) => Buffers {
                    body: BodyBuffer::new(),
                    request,
                },
                Ok(None) | Err(_) => {
                    self.suspended = Some(Suspension::Refused);
                    self.discard_window();
                    return;
                }
            },
        };
        let Some(detached) = session.detach() else {
            self.buffers = Some(buffers);
            return;
        };
        if let Some(live) = &mut self.live {
            live.first = None;
            live.jitter = None;
            live.full_at = None;
        }
        let encoded =
            match detached.encode(&self.release, installation_id, &profile, &mut buffers.body) {
                Ok(encoded) => encoded,
                Err(_) => {
                    self.totals.dropped += detached.events().len() as u64;
                    buffers.body.clear();
                    self.buffers = Some(buffers);
                    return;
                }
            };
        buffers.body.swap(&mut buffers.request.body);
        let job = JobControl::new();
        shared.publish(generation, job.clone());
        #[cfg(test)]
        if let Some(hook) = &mut self.before_handoff {
            hook();
        }
        // Immediately before the handoff: an opt-out here, or one saved by another process, sends
        // nothing.
        let cleared = !shared.quitting()
            && shared.usage.gate().is_current(generation)
            && shared.usage.confirm_stored(&self.reader, generation);
        let outcome = cleared.then(|| {
            self.last_attempt = Some(now);
            self.totals.attempts += 1;
            self.transport.send(
                &buffers.request,
                SendOptions {
                    max_request_bytes: sender::MAX_REQUEST_BYTES,
                    max_response_bytes: sender::MAX_RESPONSE_BYTES,
                    connect_timeout: sender::CONNECT_TIMEOUT,
                    read_timeout: sender::IDLE_TIMEOUT,
                    total_timeout: sender::TOTAL_TIMEOUT,
                    redirects: RedirectPolicy::default(),
                    control: &job,
                    progress: &mut |_, _| {},
                },
                &mut io::sink(),
            )
        });
        shared.unpublish();
        buffers.body.swap(&mut buffers.request.body);
        buffers.body.clear();
        self.buffers = Some(buffers);
        // The batch is discarded whatever happened.
        drop(detached);
        let Some(outcome) = outcome else {
            self.totals.dropped += encoded.sent as u64;
            return;
        };
        // An outcome after the generation ended changes nothing; the next step stops.
        if shared.quitting() || !shared.usage.gate().is_current(generation) {
            return;
        }
        let after = self.clock.monotonic();
        match sender::verdict(&outcome) {
            Verdict::Accepted => {
                session.note_sent(encoded.sent);
                self.totals.accepted += 1;
                self.failures = 0;
                self.not_before = None;
                self.cooldown = None;
            }
            Verdict::Failed => {
                self.failures = self.failures.saturating_add(1);
                let base = sender::cooldown(self.failures);
                let jitter = self
                    .randomness
                    .up_to(base * sender::COOLDOWN_JITTER_PERCENT as u32 / 100);
                self.not_before = Some(after + base + jitter);
                self.cooldown = Some(Cooldown::Failure);
            }
            Verdict::RetryAfter(wait) => {
                self.not_before = Some(after + wait);
                self.cooldown = Some(Cooldown::RateLimited);
            }
            Verdict::Suspend(reason) => self.suspended = Some(reason),
            Verdict::Cancelled => {}
        }
    }

    /// Report the counts and delivery state `usage.read` answers.
    fn publish(&self, shared: &Shared, now: Duration) {
        let mut counts = self.totals;
        if let Some(live) = &self.live {
            let session = live.session.diagnostics();
            counts.recorded += session.recorded;
            counts.dropped += session.dropped;
            counts.sent += session.sent;
        }
        counts.delivery = match (self.suspended, self.not_before) {
            _ if self.no_device.is_some() => Delivery::Suspended(Suspension::Device),
            (Some(reason), _) => Delivery::Suspended(reason),
            (None, Some(not_before)) if now < not_before => {
                Delivery::Cooldown(self.cooldown.unwrap_or(Cooldown::Failure))
            }
            _ => Delivery::Ready,
        };
        shared.usage.set_diagnostics(counts);
    }
}

/// Start the collector's session for `active`, holding its image key while the session lives.
fn start_collector(
    collector: &Collector,
    active: &Active,
    trigger: Trigger,
    started: SessionStart,
    clock: SessionClock,
) -> Arc<Session> {
    collector.start(trigger, started, clock, active.image_key.clone())
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
