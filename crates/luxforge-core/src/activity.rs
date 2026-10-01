//! The host's activity board: the long-running work in progress now, and the work that finished
//! recently.
//!
//! A worker whose work a person or an agent may be waiting on — preparing an original, developing a
//! RAW, rendering a preview, measuring a histogram — calls `ActivityBoard::begin` and holds the
//! `Activity` it gets back for as long as the work runs. A reader takes an [`ActivitySnapshot`],
//! which is the `activity.list` answer. Publishers and readers share the board's schema and nothing
//! else, so the panel that shows the work knows no worker and no worker knows the panel
//! (`docs/design/performance-panel.md`).
//!
//! The board holds current state rather than a stream of events. A reader that arrives late needs to
//! know what is running now, not the transitions it missed, and preview jobs start and finish at the
//! display's rate during a drag, which would evict the catalog mutations the event log exists for.
//! [`ActivitySnapshot::sequence`] changes whenever the contents change, so a poller can skip an
//! unchanged snapshot. A reader that should not poll at all watches the board instead
//! ([`ActivityBoard::watch`]): the next change to an entry it follows wakes it once, and it is not
//! woken again until it has read the board, so however fast the work reports, a watcher is woken at
//! most once per read and never while nothing it follows changes.
//!
//! Everything here is bounded and cheap: at most `MAX_ACTIVE` active and `MAX_RECENT` recent
//! entries and `MAX_WATCHERS` watchers, all stored in lists sized once when the board is made, one
//! uncontended mutex per call, and no timer, thread or queue.
use crate::{AssetId, Error, ErrorKind};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

/// The longest progress message any activity keeps; a longer one is cut. The one progress model
/// every publisher shares, capability jobs included (`docs/design/module-capabilities.md`).
pub(crate) const MAX_PROGRESS_MESSAGE: usize = 256;

/// How many entries can be active at once. A `begin` past this still runs its work; it records
/// nothing and counts in [`ActivitySnapshot::untracked`] instead, so a burst of work can never grow
/// the board.
pub(crate) const MAX_ACTIVE: usize = 64;

/// How many finished entries the board keeps, newest first.
pub(crate) const MAX_RECENT: usize = 16;

/// Only work that ran at least this long enters the recent list, so the preview churn of a drag,
/// whose jobs finish in a few milliseconds each, never evicts a RAW redevelopment a reader still
/// wants to see.
pub(crate) const RECENT_THRESHOLD: Duration = Duration::from_millis(250);

/// How many readers watch one board. The desktop watches its owner's board once; a watch past this
/// is refused, so a board never grows a list of watchers.
pub(crate) const MAX_WATCHERS: usize = 8;

/// What a watcher is woken with. It runs on whichever thread changed the board, while the board is
/// locked, so it must only post a signal: it never reads the board, takes a lock the board's
/// publishers hold or waits.
pub type ActivityWake = Arc<dyn Fn() + Send + Sync>;

/// Which entries a watcher follows: a change to any other entry does not wake it.
pub type ActivityFilter = fn(&ActivityEntry) -> bool;

/// How one piece of work ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Completed,
    /// Stopped before it finished: cancelled by a client, superseded by newer work, or abandoned
    /// because nothing was waiting for it any more.
    Cancelled,
    Failed,
}

impl Outcome {
    /// The outcome a worker's result records: [`ErrorKind::Cancelled`] is a cancel, any other error
    /// a failure.
    pub(crate) fn of<T>(result: &Result<T, Error>) -> Self {
        match result {
            Ok(_) => Self::Completed,
            Err(error) if error.kind == ErrorKind::Cancelled => Self::Cancelled,
            Err(_) => Self::Failed,
        }
    }
}

/// What a publisher says about the work it is beginning.
#[derive(Clone, Debug)]
pub(crate) struct ActivitySpec {
    /// A stable dotted identifier a client can switch on, such as `source.develop`.
    pub kind: &'static str,
    /// A short present-participle phrase for people, such as `Developing RAW`.
    pub label: &'static str,
    /// An optional second line for people, such as the file name being prepared.
    pub detail: Option<String>,
    pub asset_id: Option<AssetId>,
    /// The id of a job another method also answers, when the work has one.
    pub job_id: Option<String>,
}

/// How far a piece of work has come: a fraction of 0 to 1 from work that knows a truthful extent,
/// and a short message such as `downloading`. Either, both or neither may be reported at once. The
/// one progress model every activity publisher shares — a capability job reports through it rather
/// than keeping its own copy (`docs/design/module-capabilities.md`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivityProgress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ActivityProgress {
    /// Clamp the fraction to 0..=1 (dropping a non-finite one) and cut the message to
    /// [`MAX_PROGRESS_MESSAGE`] characters, so every publisher reports the same bounded shape.
    fn new(fraction: Option<f64>, message: &str) -> Self {
        Self {
            fraction: fraction
                .filter(|fraction| fraction.is_finite())
                .map(|fraction| fraction.clamp(0.0, 1.0)),
            message: (!message.is_empty())
                .then(|| message.chars().take(MAX_PROGRESS_MESSAGE).collect()),
        }
    }
}

/// What an active or a recent entry says about its work. Absent optional fields are left out of the
/// JSON rather than written as `null`.
///
/// The board's own entries borrow their publisher's static words; an entry a client reads back
/// from `activity.list` owns them. Either way it is this one type, so a client parses the answer
/// into exactly what the board wrote.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivityEntry {
    /// Unique on this board and increasing in the order the work began.
    pub id: u64,
    pub kind: Cow<'static, str>,
    pub label: Cow<'static, str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// The phase the work reported last; a finished entry keeps the one it ended in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<Cow<'static, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<ActivityProgress>,
}

/// One piece of work still running.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActiveActivity {
    #[serde(flatten)]
    pub entry: ActivityEntry,
    /// Whole milliseconds since the work began, at the moment of the snapshot.
    pub elapsed_ms: u64,
}

/// One piece of work that finished after running at least the board's recent threshold.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentActivity {
    #[serde(flatten)]
    pub entry: ActivityEntry,
    pub outcome: Outcome,
    /// Whole milliseconds from beginning to end.
    pub duration_ms: u64,
    /// Whole milliseconds since it ended, at the moment of the snapshot.
    pub ended_ms_ago: u64,
}

/// The board at one moment, as `activity.list` returns it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ActivitySnapshot {
    /// Changes exactly when the contents do, so a poller can skip a snapshot it has already seen.
    pub sequence: u64,
    /// Oldest first.
    pub active: Vec<ActiveActivity>,
    /// Newest first.
    pub recent: Vec<RecentActivity>,
    /// How many `begin` calls found the active list full, since the board was made.
    pub untracked: u64,
}

#[derive(Debug)]
struct Running {
    entry: ActivityEntry,
    started: Instant,
}

#[derive(Debug)]
struct Finished {
    entry: ActivityEntry,
    outcome: Outcome,
    started: Instant,
    ended: Instant,
}

/// One reader waiting to hear of the next change to the entries it follows.
struct Watcher {
    id: u64,
    follows: ActivityFilter,
    wake: ActivityWake,
    /// Woken by the next followed change; disarmed when woken, armed again by a read.
    armed: bool,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("id", &self.id)
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Board {
    next_id: u64,
    sequence: u64,
    untracked: u64,
    active: Vec<Running>,
    recent: VecDeque<Finished>,
    next_watcher: u64,
    watchers: Vec<Watcher>,
}

impl Board {
    /// Record that the contents changed. It wraps rather than saturating, so the sequence keeps
    /// changing with the contents however long the process runs.
    fn changed(&mut self) {
        self.sequence = self.sequence.wrapping_add(1);
    }

    fn running(&mut self, id: u64) -> Option<&mut ActivityEntry> {
        self.active
            .iter_mut()
            .find(|running| running.entry.id == id)
            .map(|running| &mut running.entry)
    }

    /// Wake the watchers that follow the active entry `id`, which just changed.
    fn notify_of(&mut self, id: u64) {
        let Self {
            active, watchers, ..
        } = self;
        if let Some(running) = active.iter().find(|running| running.entry.id == id) {
            notify(watchers, &running.entry);
        }
    }

    /// The board as a snapshot, every time in it measured against `now`.
    fn snapshot(&self, now: Instant) -> ActivitySnapshot {
        ActivitySnapshot {
            sequence: self.sequence,
            active: self
                .active
                .iter()
                .map(|running| ActiveActivity {
                    entry: running.entry.clone(),
                    elapsed_ms: whole_ms(now.saturating_duration_since(running.started)),
                })
                .collect(),
            recent: self
                .recent
                .iter()
                .map(|finished| RecentActivity {
                    entry: finished.entry.clone(),
                    outcome: finished.outcome,
                    duration_ms: whole_ms(
                        finished.ended.saturating_duration_since(finished.started),
                    ),
                    ended_ms_ago: whole_ms(now.saturating_duration_since(finished.ended)),
                })
                .collect(),
            untracked: self.untracked,
        }
    }
}

/// Wake every armed watcher that follows `entry`, which just changed, and disarm it: it is not
/// woken again until it reads the board. Nothing is allocated; each wake only posts a signal.
fn notify(watchers: &mut [Watcher], entry: &ActivityEntry) {
    for watcher in watchers
        .iter_mut()
        .filter(|watcher| watcher.armed && (watcher.follows)(entry))
    {
        watcher.armed = false;
        (watcher.wake)();
    }
}

/// One host's record of its long-running work. It is shared as `Arc<ActivityBoard>`: every worker
/// that publishes holds a clone, and so does every reader.
#[derive(Debug)]
pub struct ActivityBoard {
    board: Mutex<Board>,
    recent_threshold: Duration,
}

impl ActivityBoard {
    /// A board that keeps work of at least [`RECENT_THRESHOLD`] as recent.
    pub(crate) fn new() -> Arc<Self> {
        Self::with_recent_threshold(RECENT_THRESHOLD)
    }

    /// A board with another recent threshold. Tests lower it, usually to zero, so the short work
    /// of a small fixture is kept as recent.
    pub(crate) fn with_recent_threshold(threshold: Duration) -> Arc<Self> {
        Arc::new(Self {
            board: Mutex::new(Board {
                next_id: 1,
                sequence: 0,
                untracked: 0,
                // Sized once to their bounds, so recording work never allocates under the lock.
                active: Vec::with_capacity(MAX_ACTIVE),
                recent: VecDeque::with_capacity(MAX_RECENT + 1),
                next_watcher: 1,
                watchers: Vec::with_capacity(MAX_WATCHERS),
            }),
            recent_threshold: threshold,
        })
    }

    /// Record the beginning of one piece of work and return the guard that ends it. When
    /// [`MAX_ACTIVE`] entries are already running the guard is untracked: the work goes ahead, the
    /// board counts it in [`ActivitySnapshot::untracked`] and records nothing else about it.
    pub(crate) fn begin(self: &Arc<Self>, spec: ActivitySpec) -> Activity {
        // The entry is built before the lock is taken; inside it the board only assigns an id and
        // moves the entry into space it already has.
        let mut entry = ActivityEntry {
            id: 0,
            kind: Cow::Borrowed(spec.kind),
            label: Cow::Borrowed(spec.label),
            detail: spec.detail,
            asset_id: spec.asset_id,
            job_id: spec.job_id,
            phase: None,
            progress: None,
        };
        let started = Instant::now();
        let mut board = self.lock();
        let id = if board.active.len() < MAX_ACTIVE {
            let id = board.next_id;
            board.next_id = board.next_id.wrapping_add(1);
            entry.id = id;
            board.active.push(Running { entry, started });
            Some(id)
        } else {
            board.untracked = board.untracked.saturating_add(1);
            None
        };
        board.changed();
        if let Some(id) = id {
            board.notify_of(id);
        }
        drop(board);
        Activity {
            board: Arc::clone(self),
            id,
        }
    }

    /// The board now. It takes the lock once and copies at most [`MAX_ACTIVE`] and [`MAX_RECENT`]
    /// small entries; every time in it is measured against one clock reading taken under that lock.
    pub(crate) fn snapshot(&self) -> ActivitySnapshot {
        self.lock().snapshot(Instant::now())
    }

    /// Watch the entries `follows` picks: once the watch has read the board
    /// ([`ActivityWatch::read`]), the next begin, phase, progress or end of such an entry calls
    /// `wake`, once, and the watch is not woken again until it reads the board again. A reader that
    /// reads when it is woken, or when it chooses to, is therefore never woken faster than it reads,
    /// and nothing wakes it while nothing it follows changes. The watch starts unarmed, so its first
    /// read is what makes it hear the next change; dropping it stops the wakes. At most
    /// [`MAX_WATCHERS`] watch one board; another is `resource-limit`.
    pub fn watch(
        self: &Arc<Self>,
        follows: ActivityFilter,
        wake: ActivityWake,
    ) -> Result<ActivityWatch, Error> {
        let mut board = self.lock();
        if board.watchers.len() >= MAX_WATCHERS {
            return Err(Error::resource_limit(format!(
                "{MAX_WATCHERS} readers already watch the activity board"
            )));
        }
        let id = board.next_watcher;
        board.next_watcher = board.next_watcher.wrapping_add(1);
        board.watchers.push(Watcher {
            id,
            follows,
            wake,
            armed: false,
        });
        drop(board);
        Ok(ActivityWatch {
            board: Arc::clone(self),
            id,
        })
    }

    /// The board's state. Nothing panics while holding this lock, so a poisoned lock can only come
    /// from a panic outside it and the board is still consistent. Taking it this way also keeps a
    /// guard dropped during unwinding from panicking a second time, which would abort the process.
    fn lock(&self) -> MutexGuard<'_, Board> {
        self.board.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_phase(&self, id: u64, phase: &'static str) {
        let mut board = self.lock();
        let Some(entry) = board.running(id) else {
            return;
        };
        if entry.phase.as_deref() != Some(phase) {
            entry.phase = Some(Cow::Borrowed(phase));
            board.changed();
            board.notify_of(id);
        }
    }

    fn set_progress(&self, id: u64, progress: ActivityProgress) {
        let mut board = self.lock();
        let Some(entry) = board.running(id) else {
            return;
        };
        if entry.progress.as_ref() != Some(&progress) {
            entry.progress = Some(progress);
            board.changed();
            board.notify_of(id);
        }
    }

    /// The progress this active entry currently reports, if it has any. Reading it costs the same
    /// lock as a `set_progress`; nothing here holds the guard's own bookkeeping.
    fn progress_of(&self, id: u64) -> Option<ActivityProgress> {
        self.lock()
            .active
            .iter()
            .find(|running| running.entry.id == id)
            .and_then(|running| running.entry.progress.clone())
    }

    fn end(&self, id: u64, outcome: Outcome) {
        let ended = Instant::now();
        let mut board = self.lock();
        let Some(index) = board
            .active
            .iter()
            .position(|running| running.entry.id == id)
        else {
            return;
        };
        // `remove` rather than `swap_remove`: the active list stays oldest first.
        let Running { entry, started } = board.active.remove(index);
        board.changed();
        notify(&mut board.watchers, &entry);
        // Whatever leaves the board is dropped after the lock is released, so its strings are
        // never freed while another thread waits for the board.
        let discarded = if ended.saturating_duration_since(started) >= self.recent_threshold {
            board.recent.push_front(Finished {
                entry,
                outcome,
                started,
                ended,
            });
            if board.recent.len() > MAX_RECENT {
                board.recent.pop_back().map(|finished| finished.entry)
            } else {
                None
            }
        } else {
            Some(entry)
        };
        drop(board);
        drop(discarded);
    }
}

/// The guard for one piece of work on an [`ActivityBoard`]. Finishing it records the outcome;
/// dropping it unfinished records [`Outcome::Cancelled`], or [`Outcome::Failed`] when the thread is
/// panicking, so an entry can never outlive the work it describes.
#[must_use = "the activity ends as soon as its guard is dropped"]
#[derive(Debug)]
pub(crate) struct Activity {
    board: Arc<ActivityBoard>,
    /// `None` when the board was full and this work is untracked.
    id: Option<u64>,
}

impl Activity {
    /// Report the phase the work has reached. Reporting the phase it is already in changes nothing.
    pub(crate) fn phase(&self, phase: &'static str) {
        if let Some(id) = self.id {
            self.board.set_phase(id, phase);
        }
    }

    /// Report progress: a fraction of 0 to 1 when the work knows a truthful extent, and a short
    /// message. Either may be omitted.
    pub(crate) fn progress(&self, fraction: Option<f64>, message: &str) {
        if let Some(id) = self.id {
            self.board
                .set_progress(id, ActivityProgress::new(fraction, message));
        }
    }

    /// A handle that reports a fraction of 0 to 1 for this activity from another thread, while
    /// the guard is alive; once the activity has ended a report changes nothing. The fraction is
    /// floored to whole percent, so one piece of work changes the board at most a hundred times.
    pub(crate) fn reporter(&self) -> impl Fn(f64) + Send + Sync + 'static {
        let (board, id) = (self.board.clone(), self.id);
        move |fraction| {
            if let Some(id) = id {
                let percent = (fraction * 100.0).floor() / 100.0;
                board.set_progress(id, ActivityProgress::new(Some(percent), ""));
            }
        }
    }

    /// The progress this activity currently reports on the board, if it has any and the guard
    /// still tracks a live entry. A reader such as `job.read` uses this to answer with the
    /// same progress the board carries, rather than keeping its own copy.
    pub(crate) fn progress_snapshot(&self) -> Option<ActivityProgress> {
        self.id.and_then(|id| self.board.progress_of(id))
    }

    /// End the work with this outcome.
    pub(crate) fn finish(mut self, outcome: Outcome) {
        self.end(outcome);
    }

    fn end(&mut self, outcome: Outcome) {
        if let Some(id) = self.id.take() {
            self.board.end(id, outcome);
        }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        let outcome = if std::thread::panicking() {
            Outcome::Failed
        } else {
            Outcome::Cancelled
        };
        self.end(outcome);
    }
}

/// One reader's watch on an [`ActivityBoard`] ([`ActivityBoard::watch`]). Dropping it stops the
/// wakes.
pub struct ActivityWatch {
    board: Arc<ActivityBoard>,
    id: u64,
}

impl std::fmt::Debug for ActivityWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ActivityWatch")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl ActivityWatch {
    /// The board now — what `activity.list` answers — with the watch armed: the next change to an
    /// entry it follows wakes it. Both happen under the board's one lock, so no change can fall
    /// between the snapshot and the arming unseen and unannounced.
    pub fn read(&self) -> ActivitySnapshot {
        let mut board = self.board.lock();
        let snapshot = board.snapshot(Instant::now());
        if let Some(watcher) = board
            .watchers
            .iter_mut()
            .find(|watcher| watcher.id == self.id)
        {
            watcher.armed = true;
        }
        snapshot
    }
}

impl Drop for ActivityWatch {
    fn drop(&mut self) {
        let mut board = self.board.lock();
        let at = board
            .watchers
            .iter()
            .position(|watcher| watcher.id == self.id);
        // Removed after the lock is released, so a wake is never freed while a publisher waits.
        let removed = at.map(|at| board.watchers.remove(at));
        drop(board);
        drop(removed);
    }
}

fn whole_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn spec(kind: &'static str) -> ActivitySpec {
        ActivitySpec {
            kind,
            label: "Testing",
            detail: None,
            asset_id: None,
            job_id: None,
        }
    }

    fn keys(value: &Value) -> Vec<&str> {
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        keys
    }

    #[test]
    fn an_entry_moves_from_active_to_recent_with_its_last_phase_and_progress() {
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let asset = AssetId::new();
        let activity = board.begin(ActivitySpec {
            kind: "source.prepare",
            label: "Preparing original",
            detail: Some("DSC_0412.NEF".into()),
            asset_id: Some(asset.clone()),
            job_id: Some("source-job-7".into()),
        });
        let running = board.snapshot();
        assert_eq!(running.active.len(), 1);
        assert!(running.recent.is_empty());
        let entry = &running.active[0].entry;
        assert_eq!(entry.id, 1, "ids start at one");
        assert_eq!(
            (&*entry.kind, &*entry.label),
            ("source.prepare", "Preparing original")
        );
        assert_eq!(entry.detail.as_deref(), Some("DSC_0412.NEF"));
        assert_eq!(entry.asset_id.as_ref(), Some(&asset));
        assert_eq!(entry.job_id.as_deref(), Some("source-job-7"));
        assert_eq!((entry.phase.clone(), entry.progress.clone()), (None, None));

        activity.phase("decode");
        activity.progress(Some(0.3), "3 of 10");
        let reported = board.snapshot();
        let entry = &reported.active[0].entry;
        assert_eq!(entry.phase.as_deref(), Some("decode"));
        assert_eq!(
            entry.progress,
            Some(ActivityProgress {
                fraction: Some(0.3),
                message: Some("3 of 10".into())
            })
        );

        activity.finish(Outcome::Completed);
        let finished = board.snapshot();
        assert!(finished.active.is_empty());
        assert_eq!(finished.recent.len(), 1);
        let recent = &finished.recent[0];
        assert_eq!(recent.outcome, Outcome::Completed);
        assert_eq!(recent.entry.id, 1);
        assert_eq!(
            recent.entry.phase.as_deref(),
            Some("decode"),
            "a finished entry keeps its last phase"
        );
        assert_eq!(
            recent.entry.progress,
            Some(ActivityProgress {
                fraction: Some(0.3),
                message: Some("3 of 10".into())
            })
        );

        // Ids keep increasing across entries.
        board.begin(spec("next")).finish(Outcome::Failed);
        assert_eq!(board.snapshot().recent[0].entry.id, 2);

        // A client reads `activity.list` back into this same type, and gets exactly the board.
        let running = board.begin(spec("running"));
        running.phase("decode");
        let snapshot = board.snapshot();
        assert_eq!(
            serde_json::from_value::<ActivitySnapshot>(serde_json::to_value(&snapshot).unwrap())
                .unwrap(),
            snapshot
        );
        running.finish(Outcome::Completed);
    }

    #[test]
    fn a_dropped_guard_records_cancelled_and_a_panicking_thread_records_failed() {
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        drop(board.begin(spec("dropped")));
        let snapshot = board.snapshot();
        assert!(snapshot.active.is_empty());
        assert_eq!(snapshot.recent[0].entry.kind, "dropped");
        assert_eq!(snapshot.recent[0].outcome, Outcome::Cancelled);

        let shared = board.clone();
        let panicked = std::thread::spawn(move || {
            let _activity = shared.begin(spec("panicked"));
            panic!("the work panicked while its guard was held");
        })
        .join();
        assert!(panicked.is_err(), "the thread panicked");
        let snapshot = board.snapshot();
        assert!(
            snapshot.active.is_empty(),
            "the entry did not outlive its work"
        );
        assert_eq!(snapshot.recent[0].entry.kind, "panicked");
        assert_eq!(snapshot.recent[0].outcome, Outcome::Failed);
        // The board is still usable after the panic.
        board.begin(spec("after")).finish(Outcome::Completed);
        assert_eq!(board.snapshot().recent[0].entry.kind, "after");
    }

    #[test]
    fn a_full_board_hands_out_untracked_guards_and_counts_them() {
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let mut tracked: Vec<Activity> = (0..MAX_ACTIVE)
            .map(|_| board.begin(spec("tracked")))
            .collect();
        assert_eq!(board.snapshot().active.len(), MAX_ACTIVE);
        assert_eq!(board.snapshot().untracked, 0);

        let before = board.snapshot().sequence;
        let overflow = board.begin(spec("overflow"));
        let full = board.snapshot();
        assert_eq!(
            full.active.len(),
            MAX_ACTIVE,
            "nothing past the cap is recorded"
        );
        assert_eq!(full.untracked, 1);
        assert_ne!(
            full.sequence, before,
            "the untracked count is part of the contents"
        );
        assert!(
            full.active
                .iter()
                .all(|active| active.entry.kind == "tracked")
        );

        // An untracked guard records nothing, however it is used.
        overflow.phase("ignored");
        overflow.progress(Some(0.5), "ignored");
        overflow.finish(Outcome::Completed);
        let after = board.snapshot();
        assert_eq!(after.sequence, full.sequence);
        assert_eq!(after.untracked, 1);
        assert!(after.recent.is_empty());

        // Finishing one tracked entry frees a slot for the next begin.
        tracked.pop().unwrap().finish(Outcome::Completed);
        let next = board.begin(spec("next"));
        let snapshot = board.snapshot();
        assert_eq!(snapshot.active.len(), MAX_ACTIVE);
        assert_eq!(snapshot.active.last().unwrap().entry.kind, "next");
        assert_eq!(snapshot.untracked, 1);
        drop(next);
    }

    #[test]
    fn only_work_that_ran_at_least_the_threshold_is_recent_and_recent_keeps_the_newest_sixteen() {
        // Nothing finishes an hour after it began, so nothing here reaches the threshold: the entry
        // leaves the active list, which changes the contents, and is not kept.
        let slow = ActivityBoard::with_recent_threshold(Duration::from_secs(3600));
        slow.begin(spec("short")).finish(Outcome::Completed);
        let snapshot = slow.snapshot();
        assert!(snapshot.active.is_empty());
        assert!(
            snapshot.recent.is_empty(),
            "work below the threshold is not recent"
        );
        assert_eq!(
            snapshot.sequence, 2,
            "its begin and its end both changed the board"
        );

        // Every duration is at least zero, so at a zero threshold everything is at or above it.
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        for _ in 0..MAX_RECENT + 4 {
            board.begin(spec("work")).finish(Outcome::Completed);
        }
        let recent: Vec<u64> = board
            .snapshot()
            .recent
            .iter()
            .map(|recent| recent.entry.id)
            .collect();
        let expected: Vec<u64> = (5..=(MAX_RECENT as u64 + 4)).rev().collect();
        assert_eq!(recent, expected, "the newest sixteen, newest first");
    }

    #[test]
    fn the_sequence_changes_exactly_when_the_contents_change() {
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let mut last = board.snapshot().sequence;
        let mut step = |changed: bool, what: &str| {
            let sequence = board.snapshot().sequence;
            assert_eq!(sequence != last, changed, "{what}");
            last = sequence;
        };
        step(false, "a snapshot changes nothing");
        let activity = board.begin(spec("work"));
        step(true, "a begin");
        activity.phase("proxy");
        step(true, "a new phase");
        activity.phase("proxy");
        step(false, "the same phase again");
        activity.phase("exact");
        step(true, "another phase");
        activity.progress(Some(0.25), "quarter");
        step(true, "new progress");
        activity.progress(Some(0.25), "quarter");
        step(false, "the same progress again");
        activity.progress(Some(0.5), "half");
        step(true, "more progress");
        activity.finish(Outcome::Completed);
        step(true, "a finish");
        step(false, "reading again");
    }

    #[test]
    fn the_snapshot_serializes_to_the_activity_list_shape_and_omits_absent_keys() {
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let asset = AssetId::new();
        let bare = board.begin(spec("preview.render"));
        let full = board.begin(ActivitySpec {
            kind: "source.develop",
            label: "Developing RAW",
            detail: Some("DSC_0412.NEF".into()),
            asset_id: Some(asset.clone()),
            job_id: Some("source-job-7".into()),
        });
        full.phase("develop");
        full.progress(Some(0.5), "developing");
        let listed = serde_json::to_value(board.snapshot()).unwrap();
        assert_eq!(keys(&listed), ["active", "recent", "sequence", "untracked"]);
        let active = listed["active"].as_array().unwrap();
        assert_eq!(keys(&active[0]), ["elapsed_ms", "id", "kind", "label"]);
        assert_eq!(active[0]["kind"], json!("preview.render"));
        assert!(active[0]["elapsed_ms"].is_u64());
        assert_eq!(
            keys(&active[1]),
            [
                "asset_id",
                "detail",
                "elapsed_ms",
                "id",
                "job_id",
                "kind",
                "label",
                "phase",
                "progress"
            ]
        );
        assert_eq!(active[1]["asset_id"], json!(asset.as_str()));
        assert_eq!(
            active[1]["progress"],
            json!({"fraction": 0.5, "message": "developing"})
        );
        assert_eq!(active[1]["phase"], json!("develop"));

        drop(bare);
        full.finish(Outcome::Failed);
        let listed = serde_json::to_value(board.snapshot()).unwrap();
        assert_eq!(listed["active"], json!([]));
        let recent = listed["recent"].as_array().unwrap();
        assert_eq!(
            keys(&recent[0]),
            [
                "asset_id",
                "detail",
                "duration_ms",
                "ended_ms_ago",
                "id",
                "job_id",
                "kind",
                "label",
                "outcome",
                "phase",
                "progress"
            ]
        );
        assert_eq!(recent[0]["outcome"], json!("failed"));
        assert_eq!(
            keys(&recent[1]),
            [
                "duration_ms",
                "ended_ms_ago",
                "id",
                "kind",
                "label",
                "outcome"
            ]
        );
        assert_eq!(recent[1]["outcome"], json!("cancelled"));
        assert!(recent[1]["duration_ms"].is_u64() && recent[1]["ended_ms_ago"].is_u64());
        assert_eq!(listed["untracked"], json!(0));
    }

    /// A watch counting its wakes, following `followed` kinds only.
    fn counted(
        board: &Arc<ActivityBoard>,
        follows: ActivityFilter,
    ) -> (ActivityWatch, Arc<std::sync::atomic::AtomicUsize>) {
        let wakes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = wakes.clone();
        let watch = board
            .watch(
                follows,
                Arc::new(move || {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }),
            )
            .unwrap();
        (watch, wakes)
    }

    fn wakes(count: &std::sync::atomic::AtomicUsize) -> usize {
        count.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn catalog(entry: &ActivityEntry) -> bool {
        entry.kind.starts_with("index.")
    }

    /// A watch is woken once by the next change to an entry it follows — a begin, a phase, progress
    /// or an end — and not again until it reads the board, however much changes meanwhile; a
    /// change to an entry it does not follow never wakes it, and a watch that has not read yet
    /// hears nothing.
    #[test]
    fn a_watch_is_woken_once_per_read_by_the_entries_it_follows() {
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let (watch, woken) = counted(&board, catalog);
        let unread = board.begin(spec("index.refresh"));
        assert_eq!(wakes(&woken), 0, "unarmed until its first read");

        let snapshot = watch.read();
        assert_eq!(snapshot.active.len(), 1, "a read is the board's snapshot");
        let other = board.begin(spec("preview.render"));
        other.progress(Some(0.5), "half");
        other.finish(Outcome::Completed);
        assert_eq!(wakes(&woken), 0, "work it does not follow wakes nothing");

        unread.progress(Some(0.25), "1 of 4");
        assert_eq!(wakes(&woken), 1, "progress of followed work wakes it");
        unread.progress(Some(0.5), "2 of 4");
        unread.phase("reading headers");
        let second = board.begin(spec("index.refresh"));
        assert_eq!(wakes(&woken), 1, "not again until it reads");

        let _ = watch.read();
        second.phase("listing");
        assert_eq!(wakes(&woken), 2, "a phase wakes it");
        let _ = watch.read();
        drop(second);
        assert_eq!(wakes(&woken), 3, "an end wakes it");
        let _ = watch.read();
        let third = board.begin(spec("index.refresh"));
        assert_eq!(wakes(&woken), 4, "a begin wakes it");
        let _ = watch.read();
        unread.progress(Some(0.5), "2 of 4");
        assert_eq!(
            wakes(&woken),
            4,
            "progress that changes nothing wakes nothing"
        );
        drop((unread, third));
    }

    /// A read arms the watch under the board's lock with the snapshot it returns, so a change after
    /// the snapshot always wakes it; dropping the watch stops the wakes; and the board refuses a
    /// watch past its bound.
    #[test]
    fn a_watch_hears_every_change_after_its_read_until_it_is_dropped() {
        let board = ActivityBoard::with_recent_threshold(Duration::ZERO);
        let (watch, woken) = counted(&board, catalog);
        let before = watch.read().sequence;
        let work = board.begin(spec("index.refresh"));
        assert_ne!(board.snapshot().sequence, before);
        assert_eq!(wakes(&woken), 1, "the change after the read woke it");
        drop(watch);
        let _ = board.snapshot();
        work.finish(Outcome::Cancelled);
        assert_eq!(wakes(&woken), 1, "a dropped watch is not woken");

        let watches: Vec<ActivityWatch> = (0..MAX_WATCHERS)
            .map(|_| board.watch(catalog, Arc::new(|| {})).unwrap())
            .collect();
        let refused = board.watch(catalog, Arc::new(|| {})).unwrap_err();
        assert_eq!(refused.kind, ErrorKind::ResourceLimit);
        drop(watches);
        assert!(board.watch(catalog, Arc::new(|| {})).is_ok());
    }

    #[test]
    fn a_result_maps_to_its_outcome() {
        assert_eq!(Outcome::of(&Ok::<(), Error>(())), Outcome::Completed);
        assert_eq!(
            Outcome::of(&Err::<(), _>(Error::cancelled("stopped"))),
            Outcome::Cancelled
        );
        assert_eq!(
            Outcome::of(&Err::<(), _>(Error::internal("broke"))),
            Outcome::Failed
        );
    }

    /// What one `begin` and `finish` cost, uncontended, including the recent list's eviction at a
    /// zero threshold and the board's own `Instant` readings. Run it explicitly in release:
    /// `cargo test --release -p luxforge-core activity::tests::measure -- --ignored --nocapture`.
    #[test]
    #[ignore = "measurement, run explicitly in release"]
    fn measure_begin_and_finish() {
        const ITERATIONS: usize = 100_000;
        for (label, threshold) in [
            ("below the recent threshold", RECENT_THRESHOLD),
            ("kept as recent", Duration::ZERO),
        ] {
            let board = ActivityBoard::with_recent_threshold(threshold);
            // Warm the lock and the allocator before measuring.
            for _ in 0..1000 {
                board
                    .begin(spec("preview.render"))
                    .finish(Outcome::Completed);
            }
            let mut samples = Vec::with_capacity(ITERATIONS);
            let total = Instant::now();
            for _ in 0..ITERATIONS {
                let start = Instant::now();
                board
                    .begin(spec("preview.render"))
                    .finish(Outcome::Completed);
                samples.push(start.elapsed().as_nanos() as f64);
            }
            let mean = total.elapsed().as_nanos() as f64 / ITERATIONS as f64;
            let ns = luxforge_testbase::Distribution::of(samples).expect("iterations ran");
            println!(
                "begin+finish, {label}: p50 {:.0} ns, p95 {:.0} ns, p99 {:.0} ns, mean {mean:.0} ns including the timer, over {ITERATIONS} iterations",
                ns.p50,
                ns.p95,
                ns.percentile(99)
            );
        }
    }
}
