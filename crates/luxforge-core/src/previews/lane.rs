//! The preview lane's scheduling and workers: the priority queue of (file, tier) tasks, the
//! failures it remembers, and at most [`PREVIEW_WORKERS`] threads that make one task each at a
//! time.
//!
//! **Order.** The loupe's look-ahead first, then visible cells, then the rest of the view:
//! oldest first within the look-ahead (the loupe asks for the nearest frame first) and the rest
//! (the view's order), newest first within visible cells (the last scroll wins). A task is one
//! (file, tier): a second request joins it, and raises its priority when it asks for more; a
//! visible request refreshes its turn. At most [`PREVIEW_QUEUE_CAPACITY`] wait, and a request past
//! that is refused with `resource-limit`.
//!
//! **Workers.** Started on the first task, each blocks on its own channel while idle, so nothing
//! wakes while nothing is queued (performance rule 8). The owner hands the highest-priority task
//! to an idle worker; the worker reads the file, extracts, decodes, resamples, encodes, writes the
//! file and its row through its own connection, evicts, and posts its outcome back into the
//! owner's channel, where the owner records it and hands out the next task. The owner itself does
//! only SQL and bookkeeping (rule 5). The lane never touches the editor's source cache.
//!
//! **Developments.** A RAW with no usable preview is developed at the seam (`develop_instead`)
//! when its task was handed out at visible or look-ahead priority ([`Task::develops`]), through
//! the process's one development slot; a background task ends deferred instead
//! ([`Outcome::deferred`]), neither a tier nor a failure.
use super::{
    FILE_GRID_SIDE,
    cache::Store,
    extract::{DEVELOP, FileImages, Found, develop_instead},
};
use crate::{
    Error, ErrorKind,
    catalog_types::{
        FileId, FileSignature, LOUPE_MAX_SIDE, PreviewInfo, PreviewPriority, PreviewTier,
    },
    jobs::JobControl,
};
use std::{
    cmp::Reverse,
    collections::{BTreeSet, HashMap, VecDeque},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

/// The most tasks waiting at once: the design's 10,000 files in view, two tiers each.
pub(crate) const PREVIEW_QUEUE_CAPACITY: usize = 20_000;
/// Worker threads, started on the first task.
pub(crate) const PREVIEW_WORKERS: usize = 2;

// While one of the lane's threads (these workers and the region worker) develops, every other one
// may wait for it: the development's waiting bound admits them all, so none is ever refused.
const _: () = assert!(PREVIEW_WORKERS <= super::region::MAX_DEVELOPMENT_WAITERS);

/// The most failures remembered, one per (file, tier), the oldest forgotten first: enough for a
/// view of files none of which carries a usable preview.
pub(crate) const REMEMBERED_FAILURES: usize = PREVIEW_QUEUE_CAPACITY;

/// One task: a file's tier.
pub(crate) type TaskKey = (FileId, PreviewTier);

/// Where a waiting task stands: its priority class first, then its turn within the class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Rank {
    class: Reverse<PreviewPriority>,
    turn: u64,
}

/// What a request did to the queue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pushed {
    /// A new task.
    New,
    /// It joined a waiting task, which it moved up: a higher priority, or a newer visible turn.
    Raised,
    /// It joined a waiting task and changed nothing.
    Joined,
}

/// The waiting tasks in the order they are handed out, deduplicated by (file, tier).
#[derive(Debug)]
pub(crate) struct Queue {
    order: BTreeSet<(Rank, TaskKey)>,
    ranks: HashMap<TaskKey, (Rank, PreviewPriority)>,
    next: u64,
    capacity: usize,
}

impl Default for Queue {
    fn default() -> Self {
        Self::with_capacity(PREVIEW_QUEUE_CAPACITY)
    }
}

impl Queue {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            order: BTreeSet::new(),
            ranks: HashMap::new(),
            next: 0,
            capacity,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.ranks.len()
    }

    /// Whether `more` new tasks fit.
    pub(crate) fn fits(&self, more: usize) -> bool {
        self.len() + more <= self.capacity
    }

    fn rank(&mut self, priority: PreviewPriority) -> Rank {
        self.next += 1;
        Rank {
            class: Reverse(priority),
            // Newest first among visible cells, oldest first otherwise.
            turn: match priority {
                PreviewPriority::Visible => u64::MAX - self.next,
                PreviewPriority::Background | PreviewPriority::LookAhead => self.next,
            },
        }
    }

    /// Queue `key` at `priority`, or join it where it waits. `resource-limit` when a new task would
    /// pass the capacity.
    pub(crate) fn push(
        &mut self,
        key: TaskKey,
        priority: PreviewPriority,
    ) -> Result<Pushed, Error> {
        match self.ranks.get(&key).copied() {
            None => {
                if !self.fits(1) {
                    return Err(Error::resource_limit(format!(
                        "the preview queue holds its limit of {} tasks",
                        self.capacity
                    )));
                }
                let rank = self.rank(priority);
                self.order.insert((rank, key));
                self.ranks.insert(key, (rank, priority));
                Ok(Pushed::New)
            }
            Some((rank, held))
                if priority > held
                    || (priority == held && priority == PreviewPriority::Visible) =>
            {
                self.order.remove(&(rank, key));
                let rank = self.rank(priority);
                self.order.insert((rank, key));
                self.ranks.insert(key, (rank, priority));
                Ok(Pushed::Raised)
            }
            Some(_) => Ok(Pushed::Joined),
        }
    }

    /// The next task to hand out, and the priority it waited at.
    pub(crate) fn pop(&mut self) -> Option<(TaskKey, PreviewPriority)> {
        let (_, key) = self.order.pop_first()?;
        let (_, priority) = self.ranks.remove(&key)?;
        Some((key, priority))
    }

    /// Take `key` out of the queue; whether it was waiting.
    pub(crate) fn remove(&mut self, key: &TaskKey) -> bool {
        match self.ranks.remove(key) {
            Some((rank, _)) => {
                self.order.remove(&(rank, *key));
                true
            }
            None => false,
        }
    }
}

/// What the lane remembers of a (file, tier), with the signature the file had and the error it
/// answers: a failure, so a file that cannot give the tier is not read again until it changes,
/// or, in a second instance, a deferral, so a RAW that needs a development is not read again in
/// the background. Bounded, the oldest forgotten first, and never stored: a restart tries again.
#[derive(Debug)]
pub(crate) struct Failures {
    entries: HashMap<TaskKey, (FileSignature, Error)>,
    order: VecDeque<TaskKey>,
    capacity: usize,
}

impl Default for Failures {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity: REMEMBERED_FAILURES,
        }
    }
}

impl Failures {
    pub(crate) fn remember(&mut self, key: TaskKey, signature: FileSignature, error: Error) {
        if self.entries.insert(key, (signature, error)).is_none() {
            self.order.push_back(key);
        }
        while self.entries.len() > self.capacity {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
    }

    /// The failure remembered for `key` while the file still has `signature`.
    pub(crate) fn get(&self, key: &TaskKey, signature: &FileSignature) -> Option<&Error> {
        self.entries
            .get(key)
            .filter(|(failed, _)| failed == signature)
            .map(|(_, error)| error)
    }

    pub(crate) fn forget(&mut self, key: &TaskKey) {
        if self.entries.remove(key).is_some() {
            self.order.retain(|held| held != key);
        }
    }
}

/// A development a test runs in place of [`DEVELOP`], to count developments without a RAW.
#[cfg(test)]
pub(crate) type DevelopHook = Arc<
    dyn Fn(
            &std::path::Path,
            &FileSignature,
            u32,
            &crate::Cancel,
        ) -> Result<super::region::DevelopedPreview, Error>
        + Send
        + Sync,
>;

/// One task handed to a worker.
pub(crate) struct Task {
    pub key: TaskKey,
    /// The run's cancel flag and render token, which the lane cancels when nothing wants the task
    /// any more.
    pub control: Arc<JobControl>,
    /// The bytes the loupe and large tiers may take together.
    pub budget: u64,
    /// Whether a RAW with no usable preview is developed: a visible or look-ahead task's is, a
    /// background task's is deferred (the design's "done lazily for what is on screen").
    pub develops: bool,
    /// Where a test develops instead of [`DEVELOP`].
    #[cfg(test)]
    pub develop: Option<DevelopHook>,
    /// Where a test holds the worker before it starts the task.
    #[cfg(test)]
    pub hold: Option<Arc<luxforge_testbase::Gate>>,
    /// Where a test holds a grid task after it wrote its thumbnail stage.
    #[cfg(test)]
    pub stage_hold: Option<Arc<luxforge_testbase::Gate>>,
}

/// How a task ended.
#[derive(Debug)]
pub(crate) struct Outcome {
    pub result: Result<PreviewInfo, Error>,
    /// The index row's signature the task worked from, once it read it.
    pub signature: Option<FileSignature>,
    /// The failure holds until the file changes — no usable preview Luxforge can develop, a
    /// corrupt file, one past a limit — so the lane remembers it; a file that was unavailable or
    /// changed, a cancellation or a failed write is tried again.
    pub permanent: bool,
    /// A background task found a RAW with no usable preview and did not develop it: its result is
    /// `not-ready`, it is no failure, and a visible or look-ahead task develops it.
    pub deferred: bool,
}

/// What a worker posts back to the owner.
#[derive(Debug)]
pub(crate) enum WorkerEvent {
    /// A grid task wrote its thumbnail stage, which the grid can draw now, and goes on to the
    /// embedded preview.
    Stage { key: TaskKey },
    /// The worker finished its task and waits for the next.
    Finished {
        worker: usize,
        key: TaskKey,
        outcome: Outcome,
    },
}

/// How a worker posts into the owner's channel.
pub(crate) type Post = Arc<dyn Fn(WorkerEvent) + Send + Sync>;

/// The lane's worker threads: each blocks on its own channel while idle, and the owner sends only
/// to an idle one, so a send never waits.
pub(crate) struct Workers {
    senders: Vec<SyncSender<Task>>,
    idle: Vec<usize>,
}

impl Workers {
    /// Start one worker per store, each owning its connection to the index.
    pub(crate) fn start(stores: Vec<Store>, post: Post) -> Result<Self, Error> {
        let mut senders = Vec::with_capacity(stores.len());
        for (index, store) in stores.into_iter().enumerate() {
            let (sender, receiver) = sync_channel(1);
            let post = post.clone();
            thread::Builder::new()
                .name(format!("luxforge-preview-{index}"))
                .spawn(move || work(index, store, receiver, post))
                .map_err(|error| {
                    Error::internal(format!("cannot start a preview worker: {error}"))
                })?;
            senders.push(sender);
        }
        let idle = (0..senders.len()).rev().collect();
        Ok(Self { senders, idle })
    }

    pub(crate) fn has_idle(&self) -> bool {
        !self.idle.is_empty()
    }

    /// Hand `task` to an idle worker; it comes back when none is idle or the worker has gone, and a
    /// worker that has gone is never idle again.
    pub(crate) fn dispatch(&mut self, task: Task) -> Result<(), Task> {
        let Some(worker) = self.idle.pop() else {
            return Err(task);
        };
        self.senders[worker].send(task).map_err(|failed| failed.0)
    }

    /// Worker `worker` finished its task and is idle again.
    pub(crate) fn finished(&mut self, worker: usize) {
        if worker < self.senders.len() && !self.idle.contains(&worker) {
            self.idle.push(worker);
        }
    }
}

/// One worker: take a task, make it, post its outcome, and block for the next until the lane
/// stops, when its channel closes.
fn work(index: usize, mut store: Store, tasks: Receiver<Task>, post: Post) {
    while let Ok(task) = tasks.recv() {
        #[cfg(test)]
        if let Some(hold) = &task.hold {
            hold.pass();
        }
        let key = task.key;
        let stage = || post(WorkerEvent::Stage { key });
        // A panic in one task is that task's failure, never a worker the owner waits on forever.
        let outcome = catch_unwind(AssertUnwindSafe(|| run(&mut store, &task, &stage)))
            .unwrap_or_else(|_| Outcome {
                result: Err(Error::internal("the preview worker failed")),
                signature: None,
                permanent: false,
                deferred: false,
            });
        post(WorkerEvent::Finished {
            worker: index,
            key,
            outcome,
        });
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis().min(i64::MAX as u128) as i64)
}

/// The file at `path` is still the file the index recorded as `signature`: `source-unavailable`
/// when it is gone or has changed.
fn check_unchanged(
    path: &std::path::Path,
    signature: &FileSignature,
    when: &str,
) -> Result<(), Error> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        Error::source_unavailable(format!(
            "{} is not available: {}",
            path.display(),
            error.kind()
        ))
    })?;
    if FileSignature::of(&metadata).unchanged(signature) {
        Ok(())
    } else {
        Err(Error::source_unavailable(format!(
            "{} changed {when}",
            path.display()
        )))
    }
}

/// Make one task's tier: read the index row, remove a stale row, check the file is the one the
/// row records, make the thumbnail stage first for a grid that has none, then the tier, check the
/// file again, write it and, for a loupe tier, keep the budget. `stage` hears of the thumbnail
/// stage once it is written.
pub(crate) fn run(store: &mut Store, task: &Task, stage: &dyn Fn()) -> Outcome {
    let mut signature = None;
    let mut ended = Ended::default();
    let result = make(store, task, stage, &mut signature, &mut ended);
    Outcome {
        permanent: ended.permanent && result.is_err(),
        deferred: ended.deferred && result.is_err(),
        result,
        signature,
    }
}

/// What a task's failure means beyond its error.
#[derive(Default)]
struct Ended {
    permanent: bool,
    deferred: bool,
}

/// Whether a failure of this kind holds until the file changes, so the lane remembers it: the
/// file's own content refused it (no usable preview Luxforge can develop, a corrupt file, a colour
/// or profile it cannot read) or a size limit did. A file that is gone, changed or unreadable, a
/// cancellation or a failed write may go another way next time. A development's `resource-limit`
/// is a size limit too: the lane's threads — these workers and the region worker — are the only
/// callers of the one development, so no more of them wait than the development admits.
fn lasting(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::UnsupportedInput
            | ErrorKind::UnsupportedColor
            | ErrorKind::UnsupportedProfile
            | ErrorKind::Decode
            | ErrorKind::ResourceLimit
    )
}

fn make(
    store: &mut Store,
    task: &Task,
    stage: &dyn Fn(),
    signature: &mut Option<FileSignature>,
    ended: &mut Ended,
) -> Result<PreviewInfo, Error> {
    let (file, tier) = task.key;
    let control = &task.control;
    control.checkpoint()?;
    let record = crate::index::file(store.connection(), file)?
        .ok_or_else(|| Error::validation(format!("the index holds no file {}", file.0)))?;
    *signature = Some(record.signature);
    let cached = store.current(file, tier, &record.signature)?;
    if let Some(cached) = &cached
        && cached.complete()
    {
        // Another request made it since this task was queued.
        return Ok(cached.info());
    }
    check_unchanged(&record.path, &record.signature, "since it was indexed")?;
    let mut images = FileImages::open(&record, control).inspect_err(|error| {
        ended.permanent = lasting(error.kind);
    })?;
    if tier == PreviewTier::Grid
        && cached.is_none()
        && let Some(made) = images.thumbnail(&record, control)?
    {
        control.checkpoint()?;
        check_unchanged(
            &record.path,
            &record.signature,
            "while its previews were read",
        )?;
        store.write(file, tier, &record.signature, &made, now_ms())?;
        stage();
        #[cfg(test)]
        if let Some(hold) = &task.stage_hold {
            hold.pass();
        }
    }
    let side = match tier {
        PreviewTier::Grid => FILE_GRID_SIDE,
        _ => LOUPE_MAX_SIDE,
    };
    let found = images.preview(&record, side, control);
    // The file and its native reader go before any development, which reads the file itself.
    drop(images);
    let made = match found {
        Ok(Found::Made(made)) => made,
        Ok(Found::Unusable(why)) if !task.develops => {
            ended.deferred = true;
            return Err(Error::not_ready(format!(
                "{} has no usable preview ({why}); a Luxforge development is made only for a \
                 visible or look-ahead request",
                record.name
            )));
        }
        Ok(Found::Unusable(why)) => {
            #[cfg(test)]
            let develop = match &task.develop {
                Some(hook) => &**hook,
                None => DEVELOP,
            };
            #[cfg(not(test))]
            let develop = DEVELOP;
            develop_instead(&record, side, &why, control, develop)
                .inspect_err(|error| ended.permanent = lasting(error.kind))?
        }
        Err(error) => {
            ended.permanent = lasting(error.kind);
            return Err(error);
        }
    };
    control.checkpoint()?;
    check_unchanged(
        &record.path,
        &record.signature,
        "while its previews were read",
    )?;
    let written = store.write(file, tier, &record.signature, &made, now_ms())?;
    if tier != PreviewTier::Grid {
        store.evict(task.budget, &written.path)?;
    }
    Ok(written.info())
}
