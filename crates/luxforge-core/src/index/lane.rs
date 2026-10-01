//! The index lane: one coordinator thread that walks a root folder by folder, reconciles each
//! folder against the index by signature ([`super::reconcile`]) and writes the index in batches on
//! its own connection, [`HEADER_WORKERS`] header workers that read the headers of different files
//! ([`super::read`]), and the platform's change notifications for the indexed folders and the
//! volumes ([`luxforge_watch`]), which the coordinator applies as they arrive (`keep.rs`). Nothing
//! here runs on the catalog owner: the owner hands the lane one piece of [`Work`] at a time and
//! hears back through [`LaneEvent`]s what each batch committed, how the work ended, and what the
//! watcher reported (`api/owner/files.rs`).
//!
//! - **Off the owner, the index's opening included.** The coordinator opens the index on its first
//!   work (creating it, or recreating one it cannot use), hands the owner a connection of its own
//!   ([`LaneEvent::Opened`]), resolves the catalog's own directories and reads each root's volume
//!   from only the mounts that could hold it, so a hung network volume elsewhere never holds a
//!   listing. It owns the watcher, which it starts as it starts, so adding a root (which opens the
//!   folder, and on Linux walks it) never happens on the owner either.
//! - **One channel.** The watcher's events and the owner's word that it left work in the lane's
//!   mailbox arrive on one bounded channel of [`LANE_INPUTS`] ([`Input`]), which the coordinator
//!   blocks on. The watcher never waits on it: what a full channel refuses it folds into a rescan of
//!   the root. The owner never waits on it either: its work waits in the one-slot mailbox, and its
//!   word is only tried, since a full channel wakes the coordinator anyway, which looks in the
//!   mailbox after every input.
//! - **Bounded.** At most [`HEADERS_IN_FLIGHT`] header reads are queued or being read; a batch holds
//!   at most [`INDEX_BATCH`] writes. A listing's memory is one folder's files, the folders still to
//!   visit and the set of rows it has seen, which its file limit bounds.
//! - **Asleep when idle.** Every thread blocks on its channel; a batch waits on a receive with the
//!   batch's deadline only while it holds writes. The watcher runs no timer while nothing is owed.
//! - **Cancellable.** The walk checks the job's control between folders and every thousand entries,
//!   the workers skip the tasks of a cancelled job, and the coordinator commits the batch it holds
//!   and stops. What was committed stays; the vanished rows and the root's listing are recorded only
//!   when a listing completes.
//! - **Its own listings are jobs too.** A listing the lane runs on its own — a rescan the watcher
//!   asks for, a root listed again as it changed or as the catalog opened — is an `index-refresh`
//!   job no request started ([`Run::own_job`]): the owner opens it in its job table before it shows
//!   on the board, so `job.read` and `job.cancel` answer for it. One that does not complete leaves
//!   its root stale on its row, listed again on its next change or as the catalog next opens.
//! - **Visible.** Progress goes to the job's activity: a count while the walk is discovering the
//!   root's extent ("1,204 of about 12,408 files" when an earlier listing knows it), then a fraction
//!   of the headers read. Each committed batch advances the index's revision in the same transaction
//!   and is announced as one event ([`LaneEvent::Committed`]).
mod keep;

use super::{
    IndexDb, database,
    exclude::{Exclusions, OwnDirs},
    read::{FileTask, HeaderOutcome, read_file},
    reconcile::{Decision, Reconciler},
    volumes::{MountSource, PlatformMount, mounted_in, volume_in},
    walk::{ListedFolder, Walk, WalkLimits},
};
use crate::{
    Error, JobId,
    activity::{ActivityBoard, ActivitySpec},
    catalog_types::{
        FileId, FileRecord, HeaderState, IndexReport, IndexRoot, RootKind, Volume, VolumeId,
        jobs::INDEX_REFRESH,
    },
    editor::now_ms,
    jobs::JobControl,
};
use keep::Keeper;
pub(crate) use luxforge_watch::{VolumeEvent, WatchEvent};
use rusqlite::Connection;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Once, PoisonError,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError, sync_channel},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// The threads that read headers, each on a different file.
pub(crate) const HEADER_WORKERS: usize = 2;
/// Header reads queued to the workers or being read at once.
pub(crate) const HEADERS_IN_FLIGHT: usize = 32;
/// The most writes one batch transaction holds.
pub(crate) const INDEX_BATCH: usize = 512;
/// How long a batch collects writes before it commits, so a long listing's rows reach views as it
/// goes without one event per file.
pub(crate) const BATCH_INTERVAL: Duration = Duration::from_millis(500);
/// Inputs that may wait for the coordinator: the watcher's events (each at most
/// [`luxforge_watch::MAX_PATHS`] paths) and the owner's word. Past it the watcher folds what it
/// cannot send into one rescan of the root, so a burst larger than this costs a listing, never
/// memory.
pub(crate) const LANE_INPUTS: usize = 16;
/// How often progress is published at most.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

/// How the lane posts to the owner.
pub(crate) type Post = Arc<dyn Fn(LaneEvent) + Send + Sync>;

/// A test's gate and the folder whose work it holds.
#[cfg(any(test, feature = "test-holds"))]
pub(crate) type Hold = (Arc<luxforge_testbase::Gate>, PathBuf);

/// What the lane needs to run.
pub(crate) struct LaneConfig {
    /// The index directory. The lane opens the database in it on its first work (creating it, or
    /// recreating one it cannot use), off the owner, and hands the owner a connection of its own
    /// ([`LaneEvent::Opened`]).
    pub index_dir: PathBuf,
    /// The catalog the index belongs to.
    pub catalog_id: String,
    /// Luxforge's own directories, which the lane resolves into what every walk skips.
    pub own: OwnDirs,
    pub mounts: MountSource,
    pub limits: WalkLimits,
    pub post: Post,
    /// Where the lane's own listings, those its notifications ask for, show their progress.
    pub board: Arc<ActivityBoard>,
    /// Held at each folder of a walk, for a test that acts while a listing runs.
    #[cfg(any(test, feature = "test-holds"))]
    pub hold: Option<Hold>,
    /// Held by a header worker before each read, until its work is cancelled, for a test that
    /// acts while a read is unwritten.
    #[cfg(test)]
    pub hold_reads: Option<Hold>,
    /// The path of every header read the lane takes in, for a test that counts them.
    #[cfg(test)]
    pub reads: Arc<Mutex<Vec<PathBuf>>>,
}

/// One piece of the lane's work.
pub(crate) enum Work {
    /// An `index.refresh` job: list its roots and read what is new or changed. An indexed folder
    /// listed in full is watched from then on.
    Refresh(Refresh),
    /// Forget the root at this path and the rows under it no other root lists, as removing an
    /// indexed folder does, and stop watching it.
    Forget(PathBuf),
    /// Watch these indexed folders, each from where its notifications last stopped when the index
    /// kept a cursor for it, as the catalog opens.
    Watch(Vec<RootPlan>),
}

/// An `index.refresh` job as the lane runs it.
pub(crate) struct Refresh {
    pub job_id: JobId,
    pub control: Arc<JobControl>,
    pub roots: Vec<RootPlan>,
    /// A root that cannot be listed (offline or missing) fails the job with `source-unavailable`;
    /// otherwise the report names it and the other roots are listed.
    pub strict: bool,
}

/// One root a job lists: its path as named, why it is listed, and the volume it was last seen on.
/// A root planned as [`RootKind::Browsed`] that the index already lists as an indexed folder or a
/// card keeps that kind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RootPlan {
    pub path: PathBuf,
    pub kind: RootKind,
    pub volume_id: Option<VolumeId>,
}

/// What the lane tells the owner.
#[derive(Debug)]
pub(crate) enum LaneEvent {
    /// The lane started, with its watcher or why it has none.
    Started { watcher: Result<(), String> },
    /// The lane opened the index; this connection is the owner's, for its reads.
    Opened(IndexDb),
    /// A batch committed and left the index at `revision`, made as `by` says.
    Committed { revision: u64, by: Maker },
    /// A refresh ended, leaving the index at `revision` when the index could be read.
    Refreshed {
        job_id: JobId,
        result: Result<IndexReport, Error>,
        revision: Option<u64>,
    },
    /// The lane began a listing of its own as the `index-refresh` job `job_id`, whose cancel flag
    /// and progress are `control`'s: the owner records it running. Posted before the job shows on
    /// the activity board, so a client that saw it there finds it in the job table.
    Began {
        job_id: JobId,
        control: Arc<JobControl>,
    },
    /// A listing of the lane's own ended, leaving the index at `revision` when it could be read.
    Listed {
        job_id: JobId,
        result: Result<IndexReport, Error>,
        revision: Option<u64>,
    },
    /// A root was forgotten. One that could not be keeps its rows, a cache nothing lists.
    Forgotten,
    /// The roots of a [`Work::Watch`] are watched, or were refused as [`LaneEvent::Watching`] says.
    Watched,
    /// Whether changes under the indexed folder at `path` are followed now, or why not.
    Watching {
        path: PathBuf,
        watching: Result<(), String>,
    },
    /// A volume was mounted or taken out.
    Volume(VolumeEvent),
}

/// What made a batch, which decides under which request it is announced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum Maker {
    /// The `index-refresh` job listing: the owner's, or one of the lane's own.
    Job(JobId),
    /// The owner's other work: forgetting a folder no longer indexed.
    Owner,
    /// What the watcher reported, which no request asked for.
    #[default]
    Watcher,
}

/// What reaches the coordinator on its one channel.
pub(crate) enum Input {
    /// The owner left work in the mailbox, or asks the lane to stop.
    Wake,
    /// The watcher reports a change.
    Watch(WatchEvent),
}

impl From<WatchEvent> for Input {
    fn from(event: WatchEvent) -> Self {
        Self::Watch(event)
    }
}

/// Where the owner leaves the lane's next piece of work, and whether the lane is to stop. The
/// owner hands over one piece at a time, after the last one ended, so one slot is enough.
#[derive(Default)]
struct Mailbox {
    work: Mutex<Option<Work>>,
    stopping: AtomicBool,
}

impl Mailbox {
    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Work>> {
        self.work.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The running lane: its threads, its mailbox and the channel its inputs arrive on.
pub(crate) struct Lane {
    inputs: SyncSender<Input>,
    mailbox: Arc<Mailbox>,
    /// Cancelled as the lane stops: what it applies from its notifications stops by it, and every
    /// listing checks it beside its own job's control, so a listing the lane began on its own stops
    /// too.
    keeping: Arc<JobControl>,
    threads: Vec<JoinHandle<()>>,
}

impl Lane {
    /// Start the coordinator, with its watcher, and the header workers. They sleep until work or
    /// a notification arrives.
    pub(crate) fn start(config: LaneConfig) -> Result<Self, Error> {
        let (inputs, received) = sync_channel::<Input>(LANE_INPUTS);
        let (tasks, queued) = sync_channel::<Task>(HEADERS_IN_FLIGHT);
        let (answer, answers) = sync_channel::<Answer>(HEADERS_IN_FLIGHT);
        let queued = Arc::new(Mutex::new(queued));
        let spawn_error = |error: std::io::Error| {
            Error::internal(format!("cannot start the index lane: {error}"))
        };
        let mut threads = Vec::with_capacity(HEADER_WORKERS + 1);
        for worker in 0..HEADER_WORKERS {
            let (queued, answer) = (queued.clone(), answer.clone());
            threads.push(
                thread::Builder::new()
                    .name(format!("luxforge-index-header-{worker}"))
                    .spawn(move || header_worker(&queued, &answer))
                    .map_err(spawn_error)?,
            );
        }
        drop(answer);
        let mailbox = Arc::new(Mailbox::default());
        let keeping = JobControl::new();
        let coordinator = Coordinator {
            exclusions: Exclusions::default(),
            config,
            mailbox: mailbox.clone(),
            inputs: received,
            tasks,
            answers,
            connection: None,
            runs: 0,
            last_stamp: 0,
            keeper: Keeper::default(),
            keeping: keeping.clone(),
        };
        let watcher_inputs = inputs.clone();
        threads.push(
            thread::Builder::new()
                .name("luxforge-index".into())
                .spawn(move || coordinator.run(watcher_inputs))
                .map_err(spawn_error)?,
        );
        Ok(Self {
            inputs,
            mailbox,
            keeping,
            threads,
        })
    }

    /// Hand the lane its next piece of work. The owner sends one at a time, after the previous one
    /// ended, so the mailbox is empty and this never waits.
    pub(crate) fn send(&self, work: Work) -> Result<(), Error> {
        {
            let mut slot = self.mailbox.slot();
            if slot.is_some() {
                return Err(Error::internal(
                    "the index lane is still running earlier work",
                ));
            }
            *slot = Some(work);
        }
        self.wake();
        Ok(())
    }

    /// Tell the coordinator to look in its mailbox. A full channel refuses the word, but then the
    /// coordinator has inputs to take and looks in the mailbox after each.
    fn wake(&self) {
        let _ = self.inputs.try_send(Input::Wake);
    }

    /// Hand the coordinator an event as the watcher would, for a test.
    #[cfg(test)]
    pub(crate) fn inject(&self, event: WatchEvent) {
        let _ = self.inputs.try_send(Input::Watch(event));
    }

    /// Stop the lane once its current work ends (the owner cancels a running job first, and the
    /// lane's own listing is cancelled here), stop its watcher and wait for its threads.
    pub(crate) fn stop(self) {
        self.mailbox.stopping.store(true, Ordering::SeqCst);
        self.keeping.cancel("the catalog closed");
        self.wake();
        drop(self.inputs);
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}

/// One header read for a worker, tagged with the listing that asked for it.
struct Task {
    run: u64,
    control: Arc<JobControl>,
    file: FileTask,
    #[cfg(test)]
    hold: Option<Arc<luxforge_testbase::Gate>>,
}

/// A worker's answer: the outcome, or none when the job was cancelled before the read, tagged
/// with the listing that asked, so an answer a failed listing left behind is never taken for the
/// next one's.
struct Answer {
    run: u64,
    outcome: Option<HeaderOutcome>,
}

/// A header worker: take the next task, read it unless its job was cancelled, answer.
fn header_worker(queued: &Mutex<Receiver<Task>>, answers: &SyncSender<Answer>) {
    loop {
        let task = queued.lock().expect("the index task queue").recv();
        let Ok(task) = task else { return };
        #[cfg(test)]
        if let Some(hold) = &task.hold {
            hold.pass_unless(|| task.control.is_cancelled());
        }
        let outcome = (!task.control.is_cancelled()).then(|| {
            catch_unwind(AssertUnwindSafe(|| read_file(&task.file))).unwrap_or_else(|_| {
                HeaderOutcome::Read(Box::new(FileRecord {
                    header: HeaderState::Unreadable("reading its header failed".into()),
                    ..task.file.pending(crate::catalog_types::FileSignature {
                        len: 0,
                        modified_ns: 0,
                        identity: None,
                    })
                }))
            })
        });
        if answers
            .send(Answer {
                run: task.run,
                outcome,
            })
            .is_err()
        {
            return;
        }
    }
}

/// The coordinator's state: the index connection, the channels it works with, and the roots it
/// keeps current.
struct Coordinator {
    config: LaneConfig,
    exclusions: Exclusions,
    mailbox: Arc<Mailbox>,
    inputs: Receiver<Input>,
    tasks: SyncSender<Task>,
    answers: Receiver<Answer>,
    connection: Option<Connection>,
    /// How many runs (listings, and units of notifications) the lane has made, which tags their
    /// header reads.
    runs: u64,
    last_stamp: i64,
    keeper: Keeper,
    keeping: Arc<JobControl>,
}

impl Coordinator {
    /// Start the watcher, then take the owner's work and the watcher's events as they come, each
    /// in turn, until the lane stops. Between them the thread blocks on its channel.
    fn run(mut self, watcher_inputs: SyncSender<Input>) {
        self.exclusions = self.config.own.exclusions();
        let watcher = self.keeper.start(watcher_inputs);
        (self.config.post)(LaneEvent::Started { watcher });
        loop {
            if self.mailbox.stopping.load(Ordering::SeqCst) {
                break;
            }
            let work = self.mailbox.slot().take();
            if let Some(work) = work {
                self.work(work);
                continue;
            }
            // Asleep until an input arrives, or, while a path reported gone waits for the path it
            // may have moved to, until that wait is over.
            let input = match self.keeper.next_gone() {
                None => self
                    .inputs
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
                Some(due) => self
                    .inputs
                    .recv_timeout(due.saturating_duration_since(Instant::now())),
            };
            match input {
                Ok(Input::Wake) => {}
                Ok(Input::Watch(event)) => self.keep_up(Some(event)),
                Err(RecvTimeoutError::Timeout) => self.keep_up(None),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        // The watcher stops before the thread ends, and the header workers once `tasks` drops.
        self.keeper.stop();
    }

    /// A listing's time: later than every earlier one's, so a row it wrote is never taken for one
    /// the next listing did not see.
    fn stamp(&mut self) -> i64 {
        let stamp = now_ms().max(self.last_stamp + 1);
        self.last_stamp = stamp;
        stamp
    }

    /// One piece of the owner's work.
    fn work(&mut self, work: Work) {
        match work {
            Work::Refresh(refresh) => {
                self.runs += 1;
                let stamp = self.stamp();
                let mounts = self.config.mounts.list();
                let Self {
                    config,
                    exclusions,
                    connection,
                    tasks,
                    answers,
                    runs,
                    keeper,
                    keeping,
                    ..
                } = self;
                let (result, revision) = match open(config, connection) {
                    Ok(connection) => {
                        let mut run = Run::new(
                            *runs,
                            config,
                            exclusions,
                            connection,
                            (tasks, answers),
                            (refresh.control.clone(), keeping),
                            Maker::Job(refresh.job_id.clone()),
                            stamp,
                        );
                        // A panic ends the job `internal`, never the lane: the owner still hears
                        // how it ended, the transaction it held rolls back as it unwinds, and the
                        // workers skip what it left queued.
                        let result = catch_unwind(AssertUnwindSafe(|| {
                            run.refresh(&refresh.roots, &mounts, refresh.strict)
                        }))
                        .unwrap_or_else(|_| {
                            refresh.control.cancel("indexing failed unexpectedly");
                            Err(Error::internal("indexing failed unexpectedly"))
                        });
                        let listed = std::mem::take(&mut run.listed);
                        drop(run);
                        // An indexed folder listed in full is watched from here on.
                        keeper.listed(connection, &listed, &mounts, &config.post);
                        (result, database::revision(connection).ok())
                    }
                    Err(error) => (Err(error), None),
                };
                (config.post)(LaneEvent::Refreshed {
                    job_id: refresh.job_id,
                    result,
                    revision,
                });
            }
            Work::Forget(path) => {
                self.keeper.forget(&path);
                if let Ok(connection) = open(&self.config, &mut self.connection) {
                    let _ = forget(connection, &path, &self.config.post);
                }
                (self.config.post)(LaneEvent::Forgotten);
            }
            Work::Watch(roots) => {
                let mounts = self.config.mounts.list();
                match open(&self.config, &mut self.connection) {
                    Ok(connection) => {
                        self.keeper
                            .watch(connection, &roots, &mounts, &self.config.post);
                    }
                    Err(_) => {
                        for root in roots {
                            (self.config.post)(LaneEvent::Watching {
                                path: root.path,
                                watching: Err("the index could not be opened".into()),
                            });
                        }
                    }
                }
                (self.config.post)(LaneEvent::Watched);
            }
        }
    }

    /// Apply what the watcher reports: `first`, and the events already waiting behind it, as one
    /// unit whose writes commit in batches, as a listing's do, with the paths reported gone whose
    /// wait is over; with no `first`, only those. Once every change of the unit is
    /// written, header reads included, each root's newest cursor is recorded in the same
    /// transaction as the last of them, so a restart resumes after what was applied and never
    /// before what was not. Nothing is recorded when the unit fails; its changes are then
    /// reported again after a restart, and every root it touched is stale until it is listed
    /// again.
    fn keep_up(&mut self, first: Option<WatchEvent>) {
        self.runs += 1;
        let stamp = self.stamp();
        let mounts = self.config.mounts.list();
        let Self {
            config,
            exclusions,
            connection,
            tasks,
            answers,
            runs,
            keeper,
            keeping,
            inputs,
            last_stamp,
            ..
        } = self;
        if connection.is_none() && !config.index_dir.join(database::INDEX_FILE).exists() {
            // No index yet, so no root to keep or take offline: a volume event is only the
            // owner's to hear, and creates no index of a catalog that has listed nothing.
            if let Some(WatchEvent::Volume(event)) = first {
                (config.post)(LaneEvent::Volume(event));
            }
            return;
        }
        let Ok(connection) = open(config, connection) else {
            // Without the index nothing can be applied; the next listing of each root catches up.
            return;
        };
        let mut run = Run::new(
            *runs,
            config,
            exclusions,
            connection,
            (tasks, answers),
            (keeping.clone(), keeping),
            Maker::Watcher,
            stamp,
        );
        let outcome = catch_unwind(AssertUnwindSafe(|| -> Result<(), Error> {
            let mut next = first;
            let mut applied = 0;
            while let Some(event) = next.take() {
                keeper.apply(&mut run, event, &mounts, last_stamp)?;
                applied += 1;
                if applied >= LANE_INPUTS {
                    break;
                }
                // The events already waiting join this unit; a word from the owner ends it, so
                // its work is looked at next.
                match inputs.try_recv() {
                    Ok(Input::Watch(event)) => next = Some(event),
                    Ok(Input::Wake) | Err(_) => break,
                }
            }
            keeper.drop_gone(&mut run, &mounts)?;
            run.settle()?;
            // Reads answered after the lane began to stop were dropped unwritten: the unit's
            // cursors would skip what they came after, so they are not recorded.
            run.stop.checkpoint()?;
            keeper.record_cursors(&mut run);
            run.batch.commit(run.connection, &config.post)
        }))
        .unwrap_or_else(|_| Err(Error::internal("applying changes failed unexpectedly")));
        drop(run);
        match outcome {
            Ok(()) => keeper.recorded(),
            // Ended by the lane's stop: what was written stays and no cursor is recorded, so the
            // next start replays the unit's changes from the cursor recorded before it. (A listing
            // of its own the stop cancelled has left its root stale already.)
            Err(error) if error.kind == crate::ErrorKind::Cancelled && keeping.is_cancelled() => {
                keeper.stopped();
            }
            // What was written stays; the cursors are not recorded, and the roots the unit touched
            // are stale, listed again before any later cursor of theirs is.
            Err(_) => {
                let stale = keeper.unrecorded();
                mark_stale(connection, &stale);
            }
        }
    }
}

/// Record each of `roots` stale on its row, so it is listed again on its next change or as the
/// catalog next opens, and `index.folders` says so until then. Best effort: a root whose mark could
/// not be written is still listed again on its next change while the lane runs.
fn mark_stale(connection: &mut Connection, roots: &[PathBuf]) {
    if roots.is_empty() {
        return;
    }
    let _ = (|| -> Result<(), Error> {
        let tx = connection.transaction()?;
        for root in roots {
            database::set_root_stale(&tx, root, true)?;
        }
        Ok(tx.commit()?)
    })();
}

/// The lane's connection to the index, opening the index on the lane's first work: created when
/// there is none and recreated when it cannot be used ([`IndexDb::open`]), with a connection of
/// its own handed to the owner for its reads. A failure fails the work that needed it, and the
/// next work tries again.
fn open<'c>(
    config: &LaneConfig,
    connection: &'c mut Option<Connection>,
) -> Result<&'c mut Connection, Error> {
    if connection.is_none() {
        let (index, _) = IndexDb::open(&config.index_dir, &config.catalog_id)?;
        let own = index.connect()?;
        (config.post)(LaneEvent::Opened(index));
        *connection = Some(own);
    }
    Ok(connection.as_mut().expect("the index was opened above"))
}

/// One write of a batch.
#[derive(Debug)]
enum Write {
    /// A new file, its header still to read, or a header read.
    File(Box<FileRecord>),
    Moved {
        id: FileId,
        record: Box<FileRecord>,
        reread: bool,
    },
    Identity {
        id: FileId,
        record: Box<FileRecord>,
    },
    /// A file gone before its header was read.
    Gone(PathBuf),
    Vanished(Vec<FileId>),
    Root(IndexRoot),
    /// The root at this path was listed in full: it is no longer stale. It changes no file or
    /// root row the views read, so it advances no revision.
    Current(PathBuf),
    /// Where a watched root's notifications resume, once every change before it is written: the
    /// batch's last write, so it commits with them. It changes no file or root, so a batch of
    /// cursors alone advances no revision.
    Cursor {
        root: PathBuf,
        cursor: Option<luxforge_watch::Resume>,
    },
    /// Every root at or under a volume's mount point is offline, or online again where its folder
    /// is there, as the volume is taken out or mounted.
    Offline {
        mount_point: PathBuf,
        offline: bool,
    },
}

/// The writes collected since the last commit.
#[derive(Default)]
struct Batch {
    writes: Vec<Write>,
    since: Option<Instant>,
    /// Whether a header it holds carries a position.
    positions: bool,
    /// What makes the batches.
    by: Maker,
}

impl Batch {
    fn push(&mut self, write: Write) {
        if let Write::File(record) = &write
            && record
                .header
                .header()
                .is_some_and(|header| header.position.is_some())
        {
            self.positions = true;
        }
        self.since.get_or_insert_with(Instant::now);
        self.writes.push(write);
    }

    /// When the batch must commit, if it holds anything.
    fn deadline(&self) -> Option<Instant> {
        self.since.map(|since| since + BATCH_INTERVAL)
    }

    fn due(&self) -> bool {
        self.writes.len() >= INDEX_BATCH
            || self
                .deadline()
                .is_some_and(|deadline| Instant::now() >= deadline)
    }

    /// Write everything in one transaction that also advances the index's revision, and announce
    /// it. Nothing to write commits nothing.
    fn commit(&mut self, connection: &mut Connection, post: &Post) -> Result<(), Error> {
        if self.writes.is_empty() {
            return Ok(());
        }
        // Whether a write changed a file or a root: a cursor changes neither, and a file gone or a
        // volume's roots may find nothing to change, so a batch of nothing else advances no
        // revision and records no event.
        let mut changed = false;
        let tx = connection.transaction()?;
        for write in self.writes.drain(..) {
            changed |= match write {
                Write::File(record) => {
                    database::upsert_file(&tx, &record)?;
                    true
                }
                Write::Moved { id, record, reread } => {
                    database::move_file(&tx, id, &record, reread)?;
                    true
                }
                Write::Identity { id, record } => {
                    database::refresh_identity(
                        &tx,
                        id,
                        record.signature.identity,
                        &record.volume_id,
                    )?;
                    true
                }
                Write::Gone(path) => database::delete_file_at(&tx, &path)?,
                Write::Vanished(ids) => {
                    database::delete_files(&tx, &ids)?;
                    !ids.is_empty()
                }
                Write::Root(root) => {
                    database::upsert_root(&tx, &root)?;
                    true
                }
                Write::Current(root) => {
                    database::set_root_stale(&tx, &root, false)?;
                    false
                }
                Write::Cursor { root, cursor } => {
                    database::set_root_cursor(&tx, &root, cursor)?;
                    false
                }
                Write::Offline {
                    mount_point,
                    offline,
                } => !database::set_roots_offline_under(&tx, &mount_point, offline)?.is_empty(),
            };
        }
        let revision = changed
            .then(|| database::advance_revision(&tx))
            .transpose()?;
        tx.commit()?;
        self.since = None;
        if let Some(revision) = revision {
            post(LaneEvent::Committed {
                revision,
                by: self.by.clone(),
            });
        }
        if std::mem::take(&mut self.positions) {
            warm_gazetteer();
        }
        Ok(())
    }
}

/// Build the gazetteer's index once per process, here on the index lane after a listing committed
/// positions, so the organizer's first place lookup on the owner does not pay its ~10 ms build.
fn warm_gazetteer() {
    static WARM: Once = Once::new();
    WARM.call_once(crate::organize::Gazetteer::warm);
}

/// When progress was last published, and how many headers the job has queued.
#[derive(Default)]
struct Progress {
    published: Option<Instant>,
    queued: u32,
    answered: u32,
    /// The walk is still discovering the root's extent: header reads answered meanwhile publish
    /// nothing, because the headers queued so far are not the job's extent, and a fraction of them
    /// would read nearly done while most of the root is still unlisted. The walk's own count is
    /// what is published until it ends.
    listing: bool,
}

impl Progress {
    /// What the header reads report: the fraction of the queued headers read, once the walk has
    /// found every file it queues and so the job's extent; nothing while it is still listing.
    fn headers(&self) -> Option<(f64, String)> {
        if self.listing || self.queued == 0 {
            return None;
        }
        let fraction = f64::from(self.answered) / f64::from(self.queued);
        Some((
            fraction.min(1.0),
            format!(
                "{} of {} headers read",
                count(self.answered as usize),
                count(self.queued as usize)
            ),
        ))
    }

    fn due(&mut self) -> bool {
        let due = self
            .published
            .is_none_or(|published| published.elapsed() >= PROGRESS_INTERVAL);
        if due {
            self.published = Some(Instant::now());
        }
        due
    }
}

/// What a listing says while it is still discovering its extent: a count, and the extent an
/// earlier listing found while the count is below it ("48,210 of about 200,000 files").
fn listing(files: usize, about: Option<usize>) -> String {
    match about {
        Some(about) if about >= files => {
            format!("{} of about {} files", count(files), count(about))
        }
        _ => format!("{} files found", count(files)),
    }
}

/// A count as a sentence writes it: 48,210.
fn count(value: usize) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// One refresh, or one unit of notifications, as the coordinator runs it.
struct Run<'r> {
    /// Which of the lane's runs this is, which its tasks and their answers carry.
    id: u64,
    config: &'r LaneConfig,
    /// What every walk skips, resolved from the catalog's own directories when the lane started.
    exclusions: &'r Exclusions,
    connection: &'r mut Connection,
    tasks: &'r SyncSender<Task>,
    answers: &'r Receiver<Answer>,
    /// The control of the work running now: the refresh job's, the unit's, or the job of a listing
    /// the lane began on its own ([`Run::own_job`]) while that runs.
    control: Arc<JobControl>,
    /// The lane's own control, cancelled as the lane stops, which every listing also checks.
    stop: &'r Arc<JobControl>,
    batch: Batch,
    in_flight: usize,
    report: IndexReport,
    /// This listing's time: every row it writes is last seen then.
    stamp: i64,
    progress: Progress,
    /// The roots it listed in full, with where their notifications would resume from.
    listed: Vec<Listed>,
}

/// A root a run listed in full.
struct Listed {
    path: PathBuf,
    kind: RootKind,
    volume_id: VolumeId,
    /// The cursor read before its walk began (macOS), so a watch that resumes from it replays what
    /// changed during the walk.
    cursor: Option<luxforge_watch::Resume>,
}

/// What one walk of a folder and everything under it found.
struct Walked {
    files: usize,
    unreadable_folders: usize,
}

impl<'r> Run<'r> {
    #[allow(
        clippy::too_many_arguments,
        reason = "each is a different borrow of the coordinator"
    )]
    fn new(
        id: u64,
        config: &'r LaneConfig,
        exclusions: &'r Exclusions,
        connection: &'r mut Connection,
        (tasks, answers): (&'r SyncSender<Task>, &'r Receiver<Answer>),
        (control, stop): (Arc<JobControl>, &'r Arc<JobControl>),
        by: Maker,
        stamp: i64,
    ) -> Self {
        Self {
            id,
            config,
            exclusions,
            connection,
            tasks,
            answers,
            control,
            stop,
            batch: Batch {
                by,
                ..Batch::default()
            },
            in_flight: 0,
            report: IndexReport::default(),
            stamp,
            progress: Progress::default(),
            listed: Vec::new(),
        }
    }
}

impl Run<'_> {
    /// `Err(cancelled)` once the running work was cancelled or the lane is stopping.
    fn checkpoint(&self) -> Result<(), Error> {
        self.stop.checkpoint()?;
        self.control.checkpoint()
    }

    fn cancelled(&self) -> bool {
        self.stop.is_cancelled() || self.control.is_cancelled()
    }

    /// Run `list`, a listing the lane runs on its own under `root` — a rescan the watcher asks
    /// for, a root listed again as it changed, came back or as the catalog opened — as an
    /// `index-refresh` job no request started, named on the board by `place`, the root or the
    /// subtree it lists. The owner records the job before it shows on the activity board
    /// ([`LaneEvent::Began`]), so `job.read` and `job.cancel` answer for it as for a client's
    /// listing; it publishes its progress, its batches are announced as its own, it reports only
    /// what it listed, and the owner ends it with its report ([`LaneEvent::Listed`]). A listing that
    /// does not complete, cancelled or failed, leaves `root` stale on its row before the owner
    /// hears how it ended, so a client that reads the job ended reads the folder stale.
    fn own_job(
        &mut self,
        root: &Path,
        place: &Path,
        list: impl FnOnce(&mut Self) -> Result<(), Error>,
    ) -> Result<(), Error> {
        // What the unit read and wrote before is the unit's, not the job's: its reads in flight are
        // answered under its own control, so a cancel of the job never drops one of them.
        self.settle()?;
        self.batch.commit(self.connection, &self.config.post)?;
        let job_id = JobId::new();
        let control = JobControl::new();
        (self.config.post)(LaneEvent::Began {
            job_id: job_id.clone(),
            control: control.clone(),
        });
        control.begin_activity(self.config.board.begin(ActivitySpec {
            kind: INDEX_REFRESH.activity,
            label: INDEX_REFRESH.label,
            detail: Some(place.display().to_string()),
            asset_id: None,
            job_id: Some(job_id.to_string()),
        }));
        let unit_control = std::mem::replace(&mut self.control, control);
        let unit_maker = std::mem::replace(&mut self.batch.by, Maker::Job(job_id.clone()));
        let unit_report = std::mem::take(&mut self.report);
        let unit_progress = std::mem::take(&mut self.progress);
        let listed = list(self);
        // Its reads in flight are answered (unread once it was cancelled) and what it wrote is
        // committed as its own.
        let settled = self.settle();
        let committed = self.batch.commit(self.connection, &self.config.post);
        let result = listed.and(settled).and(committed);
        self.control = unit_control;
        self.batch.by = unit_maker;
        self.progress = unit_progress;
        let report = std::mem::replace(&mut self.report, unit_report);
        if result.is_err() {
            mark_stale(self.connection, &[root.to_path_buf()]);
        }
        let revision = database::revision(self.connection).ok();
        (self.config.post)(LaneEvent::Listed {
            job_id,
            result: result.clone().map(|()| report),
            revision,
        });
        result
    }

    /// List every root in turn. A cancel or a failure commits what the batch holds, waits for the
    /// reads in flight and stops.
    fn refresh(
        &mut self,
        roots: &[RootPlan],
        mounts: &[PlatformMount],
        strict: bool,
    ) -> Result<IndexReport, Error> {
        let mut outcome = Ok(());
        for root in roots {
            outcome = self.root(root, mounts, strict);
            if outcome.is_err() {
                break;
            }
        }
        let settled = self.settle();
        let committed = self.batch.commit(self.connection, &self.config.post);
        outcome.and(settled).and(committed)?;
        Ok(std::mem::take(&mut self.report))
    }

    /// List one root, or record why it cannot be: offline when its volume is not mounted, missing
    /// when it is gone from a mounted volume. A `strict` job fails with `source-unavailable` then;
    /// another reports it and lists the rest. Only the mounts that could hold the root, or carry
    /// its volume, are looked at in `mounts`.
    fn root(
        &mut self,
        plan: &RootPlan,
        mounts: &[PlatformMount],
        strict: bool,
    ) -> Result<(), Error> {
        self.checkpoint()?;
        let canonical = match plan.path.canonicalize() {
            Ok(canonical) => canonical,
            Err(_) => {
                let offline = plan.volume_id.as_ref().is_some_and(|volume| {
                    plan.path.symlink_metadata().is_err()
                        && mounted_in(mounts, volume, self.stamp).is_none()
                });
                if offline {
                    if let (Some(root), Some(volume_id)) = (
                        database::root(self.connection, &plan.path)?,
                        &plan.volume_id,
                    ) && !root.offline
                    {
                        self.batch.push(Write::Root(IndexRoot {
                            offline: true,
                            volume_id: volume_id.clone(),
                            ..root
                        }));
                    }
                    self.report.offline.push(plan.path.clone());
                } else {
                    self.report.missing.push(plan.path.clone());
                }
                if strict {
                    return Err(Error::source_unavailable(if offline {
                        format!(
                            "{} is offline: its volume is not connected",
                            plan.path.display()
                        )
                    } else {
                        format!("{} is not there", plan.path.display())
                    }));
                }
                return Ok(());
            }
        };
        let volume = volume_in(mounts, &canonical, self.stamp)?;
        self.report.roots.push(canonical.clone());
        let listed = database::root(self.connection, &canonical)?;
        let about = listed
            .as_ref()
            .and_then(|root| root.file_count)
            .map(|count| count as usize);
        // A folder browsed that is already listed as an indexed folder or a card stays one.
        let kind = match (plan.kind, &listed) {
            (RootKind::Browsed, Some(root)) => root.kind,
            (kind, _) => kind,
        };
        // Where an indexed folder's notifications would resume if it were watched from now, read
        // before the walk, so what changes during it is replayed. It reads no file.
        let cursor = (kind == RootKind::Indexed)
            .then(|| luxforge_watch::current_cursor(&canonical))
            .flatten();
        let walked = self.list(&canonical, &volume, about)?;
        self.batch.push(Write::Root(IndexRoot {
            path: canonical.clone(),
            kind,
            volume_id: volume.id.clone(),
            listed_ms: Some(now_ms()),
            file_count: Some(u32::try_from(walked.files).unwrap_or(u32::MAX)),
            offline: false,
        }));
        self.batch.push(Write::Current(canonical.clone()));
        self.report.files += u32::try_from(walked.files).unwrap_or(u32::MAX);
        self.report.unreadable_folders +=
            u32::try_from(walked.unreadable_folders).unwrap_or(u32::MAX);
        self.batch.commit(self.connection, &self.config.post)?;
        self.listed.push(Listed {
            path: canonical,
            kind,
            volume_id: volume.id,
            cursor,
        });
        Ok(())
    }

    /// List the folder `path` (canonical) and everything under it, on `volume`: reconcile each
    /// folder by signature, read the headers of what is new or changed, and, once the walk is
    /// complete, drop the rows under `path` it did not see. `about` is what an earlier listing
    /// found, for the progress it reports.
    fn list(
        &mut self,
        path: &Path,
        volume: &Volume,
        about: Option<usize>,
    ) -> Result<Walked, Error> {
        let mut walk = Walk::new(
            path,
            &volume.mount_point,
            self.exclusions,
            self.config.limits,
        )?;
        let (control, stop) = (self.control.clone(), self.stop.clone());
        #[cfg(any(test, feature = "test-holds"))]
        let hold = (self.config.hold.as_ref())
            .filter(|(_, under)| path.starts_with(under))
            .map(|(gate, _)| gate.clone());
        let checkpoint = move || {
            #[cfg(any(test, feature = "test-holds"))]
            if let Some(hold) = &hold {
                hold.pass_unless(|| control.is_cancelled() || stop.is_cancelled());
            }
            stop.checkpoint()?;
            control.checkpoint()
        };
        self.control.set_phase("listing");
        self.control.set_progress(None, &listing(0, about));
        self.progress.listing = true;
        let mut reconciler = Reconciler::new(volume.id.clone());
        while let Some(folder) = walk.next_folder(&checkpoint)? {
            let decisions = reconciler.folder(self.connection, &folder)?;
            self.apply_folder(&folder, &decisions, &volume.id)?;
            self.drain_ready()?;
            if self.progress.due() {
                self.control
                    .set_progress(None, &listing(walk.files(), about));
            }
        }
        self.progress.listing = false;
        self.control.set_phase("reading headers");
        self.settle()?;
        let vanished = reconciler.vanished(self.connection, path, self.stamp)?;
        self.report.removed += vanished.len() as u32;
        if !vanished.is_empty() {
            self.batch.push(Write::Vanished(vanished));
        }
        Ok(Walked {
            files: walk.files(),
            unreadable_folders: walk.unreadable_folders(),
        })
    }

    /// Queue what one folder's decisions need: rows to write and headers to read.
    fn apply_folder(
        &mut self,
        folder: &ListedFolder,
        decisions: &[Decision],
        volume: &VolumeId,
    ) -> Result<(), Error> {
        for (file, decision) in folder.files.iter().zip(decisions) {
            let task = FileTask {
                path: folder.path.join(&file.name),
                folder: folder.path.clone(),
                name: file.name.clone(),
                kind: file.kind,
                volume_id: volume.clone(),
                seen_ms: self.stamp,
            };
            match *decision {
                Decision::Unchanged => {}
                Decision::Identity(id) => self.batch.push(Write::Identity {
                    id,
                    record: Box::new(task.pending(file.signature)),
                }),
                Decision::Reread(_) => {
                    self.report.changed += 1;
                    self.read(task)?;
                }
                Decision::Moved { id, reread } => {
                    self.report.moved += 1;
                    self.batch.push(Write::Moved {
                        id,
                        record: Box::new(task.pending(file.signature)),
                        reread,
                    });
                    if reread {
                        self.read(task)?;
                    }
                }
                Decision::New => {
                    self.report.added += 1;
                    self.batch
                        .push(Write::File(Box::new(task.pending(file.signature))));
                    self.read(task)?;
                }
            }
            if self.batch.due() {
                self.batch.commit(self.connection, &self.config.post)?;
            }
        }
        Ok(())
    }

    /// Queue one header read, first taking answers while the reads in flight are at their bound.
    fn read(&mut self, file: FileTask) -> Result<(), Error> {
        while self.in_flight >= HEADERS_IN_FLIGHT {
            let answer = self
                .answers
                .recv()
                .map_err(|_| Error::internal("the index lane's header workers stopped"))?;
            self.answered(answer)?;
        }
        #[cfg(test)]
        let hold = (self.config.hold_reads.as_ref())
            .filter(|(_, under)| file.path.starts_with(under))
            .map(|(gate, _)| gate.clone());
        self.tasks
            .send(Task {
                run: self.id,
                control: self.control.clone(),
                file,
                #[cfg(test)]
                hold,
            })
            .map_err(|_| Error::internal("the index lane's header workers stopped"))?;
        self.in_flight += 1;
        self.progress.queued += 1;
        Ok(())
    }

    /// Take every answer already there, without waiting.
    fn drain_ready(&mut self) -> Result<(), Error> {
        loop {
            match self.answers.try_recv() {
                Ok(answer) => self.answered(answer)?,
                Err(TryRecvError::Empty) => return Ok(()),
                Err(TryRecvError::Disconnected) => {
                    return Err(Error::internal("the index lane's header workers stopped"));
                }
            }
        }
    }

    /// Wait for every read in flight, committing the batch when its time comes. After a cancel the
    /// workers answer the rest at once, unread, and nothing more is written.
    fn settle(&mut self) -> Result<(), Error> {
        while self.in_flight > 0 {
            let answer = match self.batch.deadline() {
                None => self
                    .answers
                    .recv()
                    .map_err(|_| RecvTimeoutError::Disconnected),
                Some(deadline) => self
                    .answers
                    .recv_timeout(deadline.saturating_duration_since(Instant::now())),
            };
            match answer {
                Ok(answer) => self.answered(answer)?,
                Err(RecvTimeoutError::Timeout) => {
                    self.batch.commit(self.connection, &self.config.post)?
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(Error::internal("the index lane's header workers stopped"));
                }
            }
        }
        Ok(())
    }

    /// One worker's answer: its record joins the batch unless the job was cancelled. An answer to
    /// an earlier listing, which failed before it took it, is dropped.
    fn answered(&mut self, answer: Answer) -> Result<(), Error> {
        if answer.run != self.id {
            return Ok(());
        }
        self.in_flight -= 1;
        self.progress.answered += 1;
        if self.cancelled() {
            return Ok(());
        }
        match answer.outcome {
            Some(HeaderOutcome::Read(record)) => {
                self.report.headers_read += 1;
                #[cfg(test)]
                self.config
                    .reads
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(record.path.clone());
                if matches!(record.header, HeaderState::Unreadable(_)) {
                    self.report.unreadable += 1;
                }
                self.batch.push(Write::File(record));
            }
            Some(HeaderOutcome::Vanished(path)) => {
                self.report.removed += 1;
                self.batch.push(Write::Gone(path));
            }
            None => {}
        }
        if let Some((fraction, message)) = self.progress.headers()
            && self.progress.due()
        {
            self.control.set_progress(Some(fraction), &message);
        }
        if self.batch.due() {
            self.batch.commit(self.connection, &self.config.post)?;
        }
        Ok(())
    }
}

/// Forget the root at `path`: its row, and the rows under it that no other root lists. Rows under
/// a root inside it (a folder browsed there) stay, and nothing is dropped when another root holds
/// it.
fn forget(connection: &mut Connection, path: &Path, post: &Post) -> Result<(), Error> {
    let roots = database::roots(connection)?;
    let others: Vec<&IndexRoot> = roots.iter().filter(|root| root.path != path).collect();
    let dropped = if others.iter().any(|root| path.starts_with(&root.path)) {
        Vec::new()
    } else {
        let inner: Vec<&Path> = others
            .iter()
            .map(|root| root.path.as_path())
            .filter(|inner| inner.starts_with(path))
            .collect();
        let mut dropped = Vec::new();
        for id in database::files_under(connection, path)? {
            if inner.is_empty()
                || database::file(connection, id)?
                    .is_none_or(|file| !inner.iter().any(|inner| file.path.starts_with(inner)))
            {
                dropped.push(id);
            }
        }
        dropped
    };
    let listed = roots.iter().any(|root| root.path == path);
    if dropped.is_empty() && !listed {
        return Ok(());
    }
    let tx = connection.transaction()?;
    database::delete_files(&tx, &dropped)?;
    database::delete_root(&tx, path)?;
    let revision = database::advance_revision(&tx)?;
    tx.commit()?;
    post(LaneEvent::Committed {
        revision,
        by: Maker::Owner,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Progress, count, listing};

    /// While the walk is still finding files, the headers queued so far are not the job's extent:
    /// a fraction of them would read nearly done with most of the root unlisted, so none is
    /// reported until the walk ends.
    #[test]
    fn header_reads_report_a_fraction_only_once_the_walk_has_listed_everything() {
        let mut progress = Progress {
            listing: true,
            queued: 14_608,
            answered: 14_577,
            ..Progress::default()
        };
        assert_eq!(progress.headers(), None, "still listing");
        progress.listing = false;
        let (fraction, message) = progress.headers().unwrap();
        assert!((fraction - 14_577.0 / 14_608.0).abs() < 1e-12);
        assert_eq!(message, "14,577 of 14,608 headers read");
        assert_eq!(Progress::default().headers(), None, "nothing queued");
    }

    #[test]
    fn counts_are_grouped_by_thousands_and_an_extent_is_only_what_was_seen() {
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(count(48_210), "48,210");
        assert_eq!(count(1_234_567), "1,234,567");
        assert_eq!(
            listing(48_210, Some(200_000)),
            "48,210 of about 200,000 files"
        );
        assert_eq!(listing(12, None), "12 files found");
        assert_eq!(
            listing(201, Some(200)),
            "201 files found",
            "past the old extent"
        );
    }
}
