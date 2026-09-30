//! The index lane: one coordinator thread that walks a root folder by folder, reconciles each
//! folder against the index by signature ([`super::reconcile`]) and writes the index in batches on
//! its own connection, and [`HEADER_WORKERS`] header workers that read the headers of different
//! files ([`super::read`]). Nothing here runs on the catalog owner: the owner hands the lane one
//! piece of [`Work`] at a time and hears back through [`LaneEvent`]s what each batch committed and
//! how the work ended (`api/owner/files.rs`).
//!
//! - **Off the owner, the index's opening included.** The coordinator opens the index on its first
//!   work (creating it, or recreating one it cannot use), hands the owner a connection of its own
//!   ([`LaneEvent::Opened`]), resolves the catalog's own directories and reads each root's volume
//!   from only the mounts that could hold it, so a hung network volume elsewhere never holds a
//!   listing.
//! - **Bounded.** At most [`HEADERS_IN_FLIGHT`] header reads are queued or being read; a batch holds
//!   at most [`INDEX_BATCH`] writes. A listing's memory is one folder's files, the folders still to
//!   visit and the set of rows it has seen, which its file limit bounds.
//! - **Asleep when idle.** Every thread blocks on its channel; a batch waits on a receive with the
//!   batch's deadline only while it holds writes.
//! - **Cancellable.** The walk checks the job's control between folders and every thousand entries,
//!   the workers skip the tasks of a cancelled job, and the coordinator commits the batch it holds
//!   and stops. What was committed stays; the vanished rows and the root's listing are recorded only
//!   when a listing completes.
//! - **Visible.** Progress goes to the job's activity: a count while the walk is discovering the
//!   root's extent ("1,204 of about 12,408 files" when an earlier listing knows it), then a fraction
//!   of the headers read. Each committed batch advances the index's revision in the same transaction
//!   and is announced as one event ([`LaneEvent::Committed`]).
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
    catalog_types::{FileId, FileRecord, HeaderState, IndexReport, IndexRoot, RootKind, VolumeId},
    editor::now_ms,
    jobs::JobControl,
};
use rusqlite::Connection;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Once,
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
/// How often progress is published at most.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

/// How the lane posts to the owner.
pub(crate) type Post = Arc<dyn Fn(LaneEvent) + Send + Sync>;

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
    /// Held at each folder of a walk, for a test that acts while a listing runs.
    #[cfg(test)]
    pub hold: Option<Arc<luxforge_testbase::Gate>>,
}

/// One piece of the lane's work.
pub(crate) enum Work {
    /// An `index.refresh` job: list its roots and read what is new or changed.
    Refresh(Refresh),
    /// Forget the root at this path and the rows under it no other root lists, as removing an
    /// indexed folder does.
    Forget(PathBuf),
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
    /// The lane opened the index; this connection is the owner's, for its reads.
    Opened(IndexDb),
    /// A batch committed and left the index at `revision`.
    Committed { revision: u64 },
    /// A refresh ended.
    Refreshed {
        job_id: JobId,
        result: Result<IndexReport, Error>,
    },
    /// A root was forgotten. One that could not be keeps its rows, a cache nothing lists.
    Forgotten,
}

/// The running lane: its threads and the channel its work arrives on.
pub(crate) struct Lane {
    work: Option<SyncSender<Work>>,
    threads: Vec<JoinHandle<()>>,
}

impl Lane {
    /// Start the coordinator and the header workers. They sleep until work arrives.
    pub(crate) fn start(config: LaneConfig) -> Result<Self, Error> {
        let (work, commands) = sync_channel(1);
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
        threads.push(
            thread::Builder::new()
                .name("luxforge-index".into())
                .spawn(move || coordinator(config, &commands, tasks, &answers))
                .map_err(spawn_error)?,
        );
        Ok(Self {
            work: Some(work),
            threads,
        })
    }

    /// Hand the lane its next piece of work. The owner sends one at a time, after the previous one
    /// ended, so this never waits.
    pub(crate) fn send(&self, work: Work) -> Result<(), Error> {
        self.work
            .as_ref()
            .ok_or_else(|| Error::internal("the index lane has stopped"))?
            .try_send(work)
            .map_err(|_| Error::internal("the index lane is still running earlier work"))
    }

    /// Stop the lane once its current work ends (the owner cancels it first) and wait for its
    /// threads.
    pub(crate) fn stop(mut self) {
        self.work = None;
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

/// One header read for a worker, tagged with the listing that asked for it.
struct Task {
    run: u64,
    control: Arc<JobControl>,
    file: FileTask,
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

/// The coordinator: run each piece of work as it arrives, and report how it ended.
fn coordinator(
    config: LaneConfig,
    commands: &Receiver<Work>,
    tasks: SyncSender<Task>,
    answers: &Receiver<Answer>,
) {
    let exclusions = config.own.exclusions();
    let mut connection = None;
    let mut last_stamp = 0;
    let mut runs = 0;
    while let Ok(work) = commands.recv() {
        match work {
            Work::Refresh(refresh) => {
                runs += 1;
                // Every listing's time is later than the last one's, so a row it wrote is never
                // taken for one the next listing did not see.
                let stamp = now_ms().max(last_stamp + 1);
                last_stamp = stamp;
                let result = match open(&config, &mut connection) {
                    Ok(connection) => {
                        let mut run = Run {
                            id: runs,
                            config: &config,
                            exclusions: &exclusions,
                            connection,
                            tasks: &tasks,
                            answers,
                            control: &refresh.control,
                            batch: Batch::default(),
                            in_flight: 0,
                            report: IndexReport::default(),
                            stamp,
                            progress: Progress::default(),
                        };
                        // A panic ends the job `internal`, never the lane: the owner still hears
                        // how it ended, the transaction it held rolls back as it unwinds, and the
                        // workers skip what it left queued.
                        catch_unwind(AssertUnwindSafe(|| {
                            run.refresh(&refresh.roots, refresh.strict)
                        }))
                        .unwrap_or_else(|_| {
                            refresh.control.cancel("indexing failed unexpectedly");
                            Err(Error::internal("indexing failed unexpectedly"))
                        })
                    }
                    Err(error) => Err(error),
                };
                (config.post)(LaneEvent::Refreshed {
                    job_id: refresh.job_id,
                    result,
                });
            }
            Work::Forget(path) => {
                if let Ok(connection) = open(&config, &mut connection) {
                    let _ = forget(connection, &path, &config.post);
                }
                (config.post)(LaneEvent::Forgotten);
            }
        }
    }
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
}

/// The writes collected since the last commit.
#[derive(Default)]
struct Batch {
    writes: Vec<Write>,
    since: Option<Instant>,
    /// Whether a header it holds carries a position.
    positions: bool,
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
        let tx = connection.transaction()?;
        for write in self.writes.drain(..) {
            match write {
                Write::File(record) => {
                    database::upsert_file(&tx, &record)?;
                }
                Write::Moved { id, record, reread } => {
                    database::move_file(&tx, id, &record, reread)?;
                }
                Write::Identity { id, record } => database::refresh_identity(
                    &tx,
                    id,
                    record.signature.identity,
                    &record.volume_id,
                )?,
                Write::Gone(path) => database::delete_file_at(&tx, &path)?,
                Write::Vanished(ids) => database::delete_files(&tx, &ids)?,
                Write::Root(root) => {
                    database::upsert_root(&tx, &root)?;
                }
            }
        }
        let revision = database::advance_revision(&tx)?;
        tx.commit()?;
        self.since = None;
        post(LaneEvent::Committed { revision });
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
}

impl Progress {
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

/// One refresh as the coordinator runs it.
struct Run<'r> {
    /// Which of the lane's listings this is, which its tasks and their answers carry.
    id: u64,
    config: &'r LaneConfig,
    /// What every walk skips, resolved from the catalog's own directories when the lane started.
    exclusions: &'r Exclusions,
    connection: &'r mut Connection,
    tasks: &'r SyncSender<Task>,
    answers: &'r Receiver<Answer>,
    control: &'r Arc<JobControl>,
    batch: Batch,
    in_flight: usize,
    report: IndexReport,
    /// This listing's time: every row it writes is last seen then.
    stamp: i64,
    progress: Progress,
}

impl Run<'_> {
    /// List every root in turn. A cancel or a failure commits what the batch holds, waits for the
    /// reads in flight and stops.
    fn refresh(&mut self, roots: &[RootPlan], strict: bool) -> Result<IndexReport, Error> {
        let mounts = self.config.mounts.list();
        let mut outcome = Ok(());
        for root in roots {
            outcome = self.root(root, &mounts, strict);
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
        self.control.checkpoint()?;
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
        let mut walk = Walk::new(
            &canonical,
            &volume.mount_point,
            self.exclusions,
            self.config.limits,
        )?;
        let control = self.control.clone();
        #[cfg(test)]
        let hold = self.config.hold.clone();
        let checkpoint = move || {
            #[cfg(test)]
            if let Some(hold) = &hold {
                hold.pass_unless(|| control.is_cancelled());
            }
            control.checkpoint()
        };
        self.control.set_phase("listing");
        self.control.set_progress(None, &listing(0, about));
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
        self.control.set_phase("reading headers");
        self.settle()?;
        let vanished = reconciler.vanished(self.connection, &canonical, self.stamp)?;
        self.report.removed += vanished.len() as u32;
        if !vanished.is_empty() {
            self.batch.push(Write::Vanished(vanished));
        }
        self.batch.push(Write::Root(IndexRoot {
            path: canonical,
            kind,
            volume_id: volume.id,
            listed_ms: Some(now_ms()),
            file_count: Some(u32::try_from(walk.files()).unwrap_or(u32::MAX)),
            offline: false,
        }));
        self.report.files += u32::try_from(walk.files()).unwrap_or(u32::MAX);
        self.report.unreadable_folders +=
            u32::try_from(walk.unreadable_folders()).unwrap_or(u32::MAX);
        self.batch.commit(self.connection, &self.config.post)
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
        self.tasks
            .send(Task {
                run: self.id,
                control: self.control.clone(),
                file,
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
        if self.control.is_cancelled() {
            return Ok(());
        }
        match answer.outcome {
            Some(HeaderOutcome::Read(record)) => {
                self.report.headers_read += 1;
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
        if self.progress.due() && self.progress.queued > 0 {
            let fraction = f64::from(self.progress.answered) / f64::from(self.progress.queued);
            self.control.set_progress(
                Some(fraction.min(1.0)),
                &format!(
                    "{} of {} headers read",
                    count(self.progress.answered as usize),
                    count(self.progress.queued as usize)
                ),
            );
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
    post(LaneEvent::Committed { revision });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{count, listing};

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
