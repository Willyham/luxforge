//! Fixed-size event records, the session that numbers and times them, and its two bounded banks.
//!
//! A producer checks the [`Gate`] (one atomic load), then writes one [`Event`] into the session's
//! active bank under a `try_lock`: no allocation (each bank's 512 records are reserved when the
//! session starts), no formatting, no filesystem or network work and no waiting. Contention or a
//! full bank drops the event and counts it; its sequence number is still consumed, so the loss shows
//! as a `seq` gap. The banks are the only event storage. A disabled process has no [`Session`] and
//! so no statistics storage at all.
//!
//! The statistics worker [`Session::detach`]es the active bank: the banks swap, producers carry on
//! into the other (empty) one, and the worker reads the detached window under its own lock, which no
//! producer ever waits for. Dropping the [`Detached`] window discards it, sent or not, so each batch
//! holds only events no earlier batch held.
//!
//! Every record takes its `seq` and `t` while holding the active bank's lock, and the worker swaps
//! banks only while holding it too, so within a window and across windows `seq` strictly increases
//! and `t` never decreases in recording order.
use super::{
    Gate, Generation,
    encode::{ReportId, SessionStart},
    image_id::ImageId,
    registry::{
        Feature, Idle, MAX_COUNT, MAX_DURATION_MS, MAX_EVENTS, MAX_MEMORY_MIB, MAX_PERCENT,
        MemoryKind, Operation, Outcome, Presented, Renderer, Tool, Trigger, Workspace,
    },
};
use std::{
    sync::{
        Arc, Mutex, MutexGuard, PoisonError, RwLock, TryLockError,
        atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

/// One recorded event: its session sequence number, milliseconds since the session started, and
/// what happened. `Copy`, fixed-size and string-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub seq: u32,
    pub t: u32,
    pub kind: EventKind,
}

/// What happened, with exactly the fields the registry allows its event name. Every field is a
/// registry enum, a bounded integer or a keyed [`ImageId`]: there is no way to put a string,
/// a path, a name or an editing value into an event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// The session's first event, at time 0.
    SessionStart {
        trigger: Trigger,
    },
    AppPresented {
        state: Presented,
    },
    AppIdle {
        state: Idle,
    },
    WorkspaceEnter {
        workspace: Workspace,
    },
    ImageOpen {
        image: ImageId,
    },
    Interaction {
        feature: Feature,
        image: Option<ImageId>,
    },
    ToolCommit {
        tool: Tool,
        image: ImageId,
    },
    ImagesAdded {
        count: AddedCount,
    },
    OperationEnd(OperationEnd),
    PreviewSettled {
        image: ImageId,
        renderer: Renderer,
        duration: DurationMs,
    },
    ProcessSample(ProcessSample),
}

impl EventKind {
    pub fn name(&self) -> super::registry::EventName {
        use super::registry::EventName as N;
        match self {
            Self::SessionStart { .. } => N::SessionStart,
            Self::AppPresented { .. } => N::AppPresented,
            Self::AppIdle { .. } => N::AppIdle,
            Self::WorkspaceEnter { .. } => N::WorkspaceEnter,
            Self::ImageOpen { .. } => N::ImageOpen,
            Self::Interaction { .. } => N::Interaction,
            Self::ToolCommit { .. } => N::ToolCommit,
            Self::ImagesAdded { .. } => N::ImagesAdded,
            Self::OperationEnd(_) => N::OperationEnd,
            Self::PreviewSettled { .. } => N::PreviewSettled,
            Self::ProcessSample(_) => N::ProcessSample,
        }
    }
}

/// A duration in milliseconds, clipped to 24 hours.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct DurationMs(u32);

impl DurationMs {
    /// `millis`, clipped to 24 hours.
    pub fn from_millis(millis: u64) -> Self {
        Self(millis.min(u64::from(MAX_DURATION_MS)) as u32)
    }

    pub fn from_duration(duration: Duration) -> Self {
        Self::from_millis(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
    }

    /// Exactly `millis` if it is in range: the validator's reading.
    pub fn exact(millis: u64) -> Option<Self> {
        (millis <= u64::from(MAX_DURATION_MS)).then_some(Self(millis as u32))
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

/// A percentage of one core (or of the GPU), an integer clipped to `0..=102400`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Percent(u32);

impl Percent {
    /// The rounded observation, clipped; `None` for a nonfinite or negative one, which is dropped
    /// rather than reported as zero.
    pub fn from_f64(percent: f64) -> Option<Self> {
        if !percent.is_finite() || percent < 0.0 {
            return None;
        }
        Some(Self(percent.round().min(f64::from(MAX_PERCENT)) as u32))
    }

    /// Exactly `percent` if it is in range: the validator's reading.
    pub fn exact(percent: u64) -> Option<Self> {
        (percent <= u64::from(MAX_PERCENT)).then_some(Self(percent as u32))
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

/// A memory figure in MiB, clipped to `0..=16777216`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemoryMib(u32);

impl MemoryMib {
    /// `bytes` in whole MiB, rounded down and clipped.
    pub fn from_bytes(bytes: u64) -> Self {
        Self((bytes >> 20).min(u64::from(MAX_MEMORY_MIB)) as u32)
    }

    /// Exactly `mib` if it is in range: the validator's reading.
    pub fn exact(mib: u64) -> Option<Self> {
        (mib <= u64::from(MAX_MEMORY_MIB)).then_some(Self(mib as u32))
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

/// How many original photographs one add committed: `1..=1000000`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct AddedCount(u32);

impl AddedCount {
    /// `count`, clipped to a million; `None` for zero, which is not an add.
    pub fn new(count: u64) -> Option<Self> {
        (count > 0).then(|| Self(count.min(u64::from(MAX_COUNT)) as u32))
    }

    /// Exactly `count` if it is in range: the validator's reading.
    pub fn exact(count: u64) -> Option<Self> {
        (1..=u64::from(MAX_COUNT))
            .contains(&count)
            .then_some(Self(count as u32))
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

/// The terminal outcome of one fixed operation. Its constructors allow `count` only on an export
/// and `image` only on source preparation, Auto tone and an export.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperationEnd {
    operation: Operation,
    outcome: Outcome,
    duration: DurationMs,
    count: Option<u32>,
    image: Option<ImageId>,
}

impl OperationEnd {
    /// Preparing the open photograph's source.
    pub fn source_preparation(
        outcome: Outcome,
        duration: DurationMs,
        image: Option<ImageId>,
    ) -> Self {
        Self::new(Operation::SourcePreparation, outcome, duration, None, image)
    }

    /// A catalog scan: no image, no count.
    pub fn catalog_scan(outcome: Outcome, duration: DurationMs) -> Self {
        Self::new(Operation::CatalogScan, outcome, duration, None, None)
    }

    /// An export: `written` images written (clipped to a million), and `image` when it exported the
    /// open Develop photograph alone.
    pub fn export(
        outcome: Outcome,
        duration: DurationMs,
        written: Option<u64>,
        image: Option<ImageId>,
    ) -> Self {
        let count = written.map(|n| n.min(u64::from(MAX_COUNT)) as u32);
        Self::new(Operation::Export, outcome, duration, count, image)
    }

    /// Auto tone on the open photograph.
    pub fn auto_tone(outcome: Outcome, duration: DurationMs, image: Option<ImageId>) -> Self {
        Self::new(Operation::AutoTone, outcome, duration, None, image)
    }

    /// The validator's reading: `None` when a field is not allowed on the operation or is out of
    /// range.
    pub fn checked(
        operation: Operation,
        outcome: Outcome,
        duration: DurationMs,
        count: Option<u64>,
        image: Option<ImageId>,
    ) -> Option<Self> {
        if (count.is_some() && !operation.may_carry_count())
            || (image.is_some() && !operation.may_carry_image())
            || count.is_some_and(|n| n > u64::from(MAX_COUNT))
        {
            return None;
        }
        Some(Self::new(
            operation,
            outcome,
            duration,
            count.map(|n| n as u32),
            image,
        ))
    }

    fn new(
        operation: Operation,
        outcome: Outcome,
        duration: DurationMs,
        count: Option<u32>,
        image: Option<ImageId>,
    ) -> Self {
        Self {
            operation,
            outcome,
            duration,
            count,
            image,
        }
    }

    pub fn operation(&self) -> Operation {
        self.operation
    }

    pub fn outcome(&self) -> Outcome {
        self.outcome
    }

    pub fn duration(&self) -> DurationMs {
        self.duration
    }

    pub fn count(&self) -> Option<u32> {
        self.count
    }

    pub fn image(&self) -> Option<ImageId> {
        self.image
    }
}

/// One process performance sample. A counter that is missing, reset or invalid is `None`, never a
/// synthetic zero; with no valid CPU or memory figure there is no sample.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessSample {
    /// Percent of one core from CPU-time deltas, not normalized to the core count.
    pub cpu: Percent,
    pub memory: MemoryMib,
    pub memory_kind: MemoryKind,
    pub gpu: Option<Percent>,
    pub gpu_memory: Option<MemoryMib>,
}

/// What a producer's call did. Producers normally ignore it; the session counts every outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Collection is off (or the producer's generation has ended): nothing was stored or counted.
    Off,
    /// Stored as this sequence number.
    Stored(u32),
    /// Dropped for contention or a full bank; a sequence number was consumed.
    Dropped,
}

/// Where a session's `t` comes from: the platform's monotonic clock, or a clock a test sets.
///
/// `Instant` does not advance while macOS sleeps, so the supervisor starts a new session after a
/// system sleep rather than let a session's times silently compress.
#[derive(Clone, Debug)]
pub enum SessionClock {
    Monotonic(Instant),
    /// Milliseconds since the session started, as a test or simulation sets them.
    Manual(Arc<AtomicU32>),
}

impl SessionClock {
    /// The monotonic clock, starting now.
    pub fn monotonic() -> Self {
        Self::Monotonic(Instant::now())
    }

    /// A manual clock at 0 and the handle that moves it.
    pub fn manual() -> (Self, Arc<AtomicU32>) {
        let handle = Arc::new(AtomicU32::new(0));
        (Self::Manual(handle.clone()), handle)
    }

    /// Milliseconds since the session started, saturating at the wire's `u32` range.
    pub fn now_ms(&self) -> u32 {
        match self {
            Self::Monotonic(start) => {
                u32::try_from(start.elapsed().as_millis()).unwrap_or(u32::MAX)
            }
            Self::Manual(millis) => millis.load(Ordering::Acquire),
        }
    }
}

/// Local diagnostics `usage.read` may report: counts only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Diagnostics {
    pub recorded: u64,
    pub dropped: u64,
    pub sent: u64,
}

/// One session: its random ID, start time, sequence counters and two banks of at most
/// [`MAX_EVENTS`] records. Created when collection becomes effective in a launch, never persisted.
#[derive(Debug)]
pub struct Session {
    generation: Generation,
    id: ReportId,
    started: SessionStart,
    clock: SessionClock,
    /// The next sequence number; consumed by every stored and every dropped event.
    next_seq: AtomicU64,
    next_batch_seq: AtomicU64,
    /// Which bank producers write: 0 or 1.
    active: AtomicUsize,
    banks: [Mutex<Vec<Event>>; 2],
    /// Events dropped since the active window began, which its batch reports in `dropped`.
    window_dropped: AtomicU32,
    recorded: AtomicU64,
    dropped: AtomicU64,
    sent: AtomicU64,
}

impl Session {
    /// A new session in `generation` with a fresh random ID, whose first event is `session.start`
    /// at time 0 (as `clock` reads at this moment).
    pub fn new(
        generation: Generation,
        trigger: Trigger,
        started: SessionStart,
        clock: SessionClock,
    ) -> Self {
        let session = Self {
            generation,
            id: ReportId::random(),
            started,
            clock,
            next_seq: AtomicU64::new(0),
            next_batch_seq: AtomicU64::new(0),
            active: AtomicUsize::new(0),
            banks: [
                Mutex::new(Vec::with_capacity(MAX_EVENTS)),
                Mutex::new(Vec::with_capacity(MAX_EVENTS)),
            ],
            window_dropped: AtomicU32::new(0),
            recorded: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            sent: AtomicU64::new(0),
        };
        let mut first = lock(&session.banks[0]);
        session.next_seq.store(1, Ordering::Relaxed);
        first.push(Event {
            seq: 0,
            t: 0,
            kind: EventKind::SessionStart { trigger },
        });
        drop(first);
        session.recorded.store(1, Ordering::Relaxed);
        session
    }

    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// The in-memory session ID the envelope carries.
    pub fn id(&self) -> ReportId {
        self.id
    }

    /// The UTC wall-clock second the session began.
    pub fn started(&self) -> SessionStart {
        self.started
    }

    /// Milliseconds since the session started.
    pub fn now_ms(&self) -> u32 {
        self.clock.now_ms()
    }

    /// Store one event: constant, non-blocking work. See the module's description.
    pub fn record(&self, kind: EventKind) -> Recorded {
        let active = self.active.load(Ordering::Acquire);
        let mut bank = match self.banks[active].try_lock() {
            Ok(bank) => bank,
            Err(TryLockError::WouldBlock | TryLockError::Poisoned(_)) => return self.drop_event(),
        };
        // The worker swapped banks between the load and the lock: this one is detached.
        if self.active.load(Ordering::Acquire) != active {
            drop(bank);
            return self.drop_event();
        }
        let Ok(seq) = u32::try_from(self.next_seq.fetch_add(1, Ordering::Relaxed)) else {
            drop(bank);
            return self.count_drop();
        };
        if bank.len() >= MAX_EVENTS {
            drop(bank);
            return self.count_drop();
        }
        let t = self.clock.now_ms();
        // Within the capacity reserved at start, so this never reallocates.
        bank.push(Event { seq, t, kind });
        drop(bank);
        self.recorded.fetch_add(1, Ordering::Relaxed);
        Recorded::Stored(seq)
    }

    fn drop_event(&self) -> Recorded {
        self.next_seq.fetch_add(1, Ordering::Relaxed);
        self.count_drop()
    }

    fn count_drop(&self) -> Recorded {
        let _ = self
            .window_dropped
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1));
        self.dropped.fetch_add(1, Ordering::Relaxed);
        Recorded::Dropped
    }

    /// The active window's size and its oldest event's time, for the worker's flush and expiry
    /// schedule. Briefly takes the active bank's lock.
    pub fn window(&self) -> Window {
        let bank = lock(&self.banks[self.active.load(Ordering::Acquire)]);
        Window {
            len: bank.len(),
            first_t: bank.first().map(|event| event.t),
            dropped: self.window_dropped.load(Ordering::Acquire),
        }
    }

    /// Detach the active window for the worker: `None` when it is empty (an empty batch is never
    /// sent) or the previous detached window is still held. The detached window takes the next
    /// `batch_seq`, so a window discarded unsent leaves a `batch_seq` gap.
    pub fn detach(&self) -> Option<Detached<'_>> {
        let active = self.active.load(Ordering::Acquire);
        let current = lock(&self.banks[active]);
        let other = 1 - active;
        let mut next = match self.banks[other].try_lock() {
            Ok(next) => next,
            Err(_) => return None,
        };
        // An empty window is never sent; its drops carry over into the next window's count.
        if current.is_empty() {
            return None;
        }
        next.clear();
        drop(next);
        self.active.store(other, Ordering::Release);
        let dropped = self.window_dropped.swap(0, Ordering::AcqRel);
        let batch_seq = self.next_batch_seq.fetch_add(1, Ordering::Relaxed);
        Some(Detached {
            session: self,
            events: current,
            dropped,
            batch_seq: u32::try_from(batch_seq).unwrap_or(u32::MAX),
        })
    }

    /// Record that `count` events were accepted by the receiver.
    pub fn note_sent(&self, count: usize) {
        self.sent.fetch_add(count as u64, Ordering::Relaxed);
    }

    pub fn diagnostics(&self) -> Diagnostics {
        Diagnostics {
            recorded: self.recorded.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            sent: self.sent.load(Ordering::Relaxed),
        }
    }

    /// The allocation capacity this session owns: itself and both banks' reserved records.
    pub fn storage_bytes(&self) -> usize {
        let banks: usize = self
            .banks
            .iter()
            .map(|bank| lock(bank).capacity() * size_of::<Event>())
            .sum();
        size_of::<Self>() + banks
    }
}

/// The active window, as [`Session::window`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub len: usize,
    pub first_t: Option<u32>,
    pub dropped: u32,
}

/// A detached window, held by the worker while it encodes and sends it. Dropping it discards its
/// events, keeping the bank's reserved capacity.
#[derive(Debug)]
pub struct Detached<'a> {
    session: &'a Session,
    events: MutexGuard<'a, Vec<Event>>,
    dropped: u32,
    batch_seq: u32,
}

impl Detached<'_> {
    /// The window's events in recording order: strictly increasing `seq`, non-decreasing `t`.
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Events dropped while this window was active.
    pub fn dropped(&self) -> u32 {
        self.dropped
    }

    pub fn batch_seq(&self) -> u32 {
        self.batch_seq
    }

    pub fn session(&self) -> &Session {
        self.session
    }

    /// The oldest event's time, for the 30-minute expiry.
    pub fn first_t(&self) -> Option<u32> {
        self.events.first().map(|event| event.t)
    }

    /// Serialize this window as one batch into `body`. Events the body limit omits are counted as
    /// dropped in the session's diagnostics too.
    pub fn encode(
        &self,
        release: &super::encode::Release,
        installation_id: ReportId,
        device: &super::device_profile::DeviceProfile,
        body: &mut super::encode::BodyBuffer,
    ) -> Result<super::encode::Encoded, super::encode::EncodeError> {
        let envelope = super::encode::Envelope {
            release,
            installation_id,
            session_id: self.session.id,
            session_start: self.session.started,
            batch_seq: self.batch_seq,
            device,
        };
        let encoded = super::encode::encode_batch(&envelope, &self.events, self.dropped, body)?;
        self.session
            .dropped
            .fetch_add(encoded.omitted as u64, Ordering::Relaxed);
        Ok(encoded)
    }
}

impl Drop for Detached<'_> {
    fn drop(&mut self) {
        self.events.clear();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The process-wide collection point producers record into: the consent [`Gate`] and, while
/// collection is effective, the current [`Session`]. `const`-constructible, so a host may keep one
/// in a `static`; an idle or disabled collector owns no heap storage.
#[derive(Debug, Default)]
pub struct Collector {
    gate: Gate,
    session: RwLock<Option<Arc<Session>>>,
}

impl Collector {
    pub const fn new() -> Self {
        Self {
            gate: Gate::new(),
            session: RwLock::new(None),
        }
    }

    pub fn gate(&self) -> &Gate {
        &self.gate
    }

    /// Start a session: turns collection on (in a new generation if it was off) and allocates the
    /// session's banks, replacing any earlier session (after a system sleep, say).
    pub fn start(
        &self,
        trigger: Trigger,
        started: SessionStart,
        clock: SessionClock,
    ) -> Arc<Session> {
        let generation = self.gate.enable();
        let session = Arc::new(Session::new(generation, trigger, started, clock));
        *self.session.write().unwrap_or_else(PoisonError::into_inner) = Some(session.clone());
        session
    }

    /// Turn collection off at once: the generation ends, producers stop, and the session's banks
    /// are freed when the worker lets go of its handle.
    pub fn stop(&self) {
        self.gate.disable();
        self.session
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    /// The current session, for the worker.
    pub fn session(&self) -> Option<Arc<Session>> {
        self.session
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Record one event if collection is on. Non-blocking: a session being replaced or stopped
    /// drops the event (or records nothing when off).
    pub fn record(&self, kind: EventKind) -> Recorded {
        match self.gate.current() {
            Some(generation) => self.record_in(generation, kind),
            None => Recorded::Off,
        }
    }

    /// Record one event only while `generation`, the one a long operation started in, is still
    /// the current one: a completion from before an opt-in, or after a revocation, records nothing.
    pub fn record_in(&self, generation: Generation, kind: EventKind) -> Recorded {
        if !self.gate.is_current(generation) {
            return Recorded::Off;
        }
        let Ok(slot) = self.session.try_read() else {
            return Recorded::Off;
        };
        match slot.as_deref() {
            Some(session) if session.generation == generation => session.record(kind),
            _ => Recorded::Off,
        }
    }
}

#[cfg(test)]
#[path = "event_tests.rs"]
mod tests;
