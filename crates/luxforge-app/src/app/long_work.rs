//! Long-running work ([catalog design](../../../../docs/design/catalog.md#long-running-work), P18):
//! every catalog job over a second shown with its progress and Cancel, the busiest one in the
//! status bar, a progress sheet only in a view with nothing to show yet, and a sentence in the
//! status bar when a job ends. The model is `state/long_work.rs`; this seam reads the board, sends
//! `job.cancel` and follows the view waiting on a job.
//!
//! **Waking without polling.** The desktop watches its owner's activity board
//! ([`luxforge_core::activity::ActivityBoard::watch`]) for catalog work alone: the next begin,
//! phase, progress or end of a catalog job wakes it once, through one signal, and the board is not
//! read — and the watch not armed again — until the desktop reads it. With nothing running nothing
//! wakes it, and the update loop does not run for long work at all. Reading is one lock and a copy of
//! the board on the update loop (the `activity.list` answer, without an owner round trip), taken
//! with the watch's re-arming under the same lock, so no change falls between a read and the next
//! wake. Two timers exist, each only while it has something to do:
//!
//! - **The throttle.** A wake that comes within [`MIN_INTERVAL`] of the last read marks a read due
//!   and it is taken by a timer [`MIN_INTERVAL`] later, which exists only while a read is due. Work
//!   that reports twenty times a second is therefore read at most four times a second.
//! - **The refresh.** While a catalog job runs, the board is read every [`REFRESH_INTERVAL`] even
//!   when nothing changes, because the rules that follow elapsed time — a job shown once it has run
//!   half a second, an estimate withdrawn when a job stalls — can only see time pass. It stops with
//!   the last job or when presentation is hidden. Hidden watches keep lifecycle wakes and suppress
//!   progress-only wakes; restore reconciles the current board once.
//!
//! **One board for the status bar and the section.** The Performance section's job rows are drawn
//! from the board read here, and its own sampler reads the board through this watch at each of its
//! ticks ([`Editor::section_reads_board`]) rather than asking the owner for `activity.list`, so the
//! status bar's job and the section's rows always come from the same read. That read is the
//! section's, on its own timer that exists only while it samples: it adds no timer here and counts
//! in the section's reads, not in [`LongWork::reads`].
//!
//! **The view waiting on a job.** Select's first look at a folder waits for its `index.refresh`
//! job. Independent authoritative `job.wait` readers keep first-look, folder-add and Locate results
//! live while hidden, including partial search answers and final owner publication after an
//! activity has ended. The board also retains the existing visible follow-up routes.
use crate::app::{
    Before, Editor,
    message::{Message, long_work::LongWorkMessage, view::ViewMessage},
    outcome::Outcome,
    select::job_now,
    tasks::{call, owner_task},
    waker::Signal,
};
use crate::state::{
    long_work::{FINISHED_SENTENCE_MS, LongWorkState, Waiting, finished_sentence, followed},
    palette::Panel,
    select::ReadSource,
};
use iced::futures::{
    StreamExt,
    stream::{self, BoxStream},
};
use iced::{Subscription, Task};
use luxforge_core::{
    ClientId, OwnerHandle,
    activity::{ActivityWatch, RecentActivity},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

/// The least time between two reads of the board: at most four a second while work reports.
pub(crate) const MIN_INTERVAL: Duration = Duration::from_millis(250);

/// How often the board is read while a catalog job runs and nothing else reads it: the resolution
/// of the rules that follow elapsed time.
pub(crate) const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// The most Cancel presses remembered for evidence.
const CANCELS_KEPT: usize = 8;

/// The board's kind of the index lane's listings: a card listed as it is mounted, an indexed folder
/// listed, a card or folder read for a client.
const LISTING: &str = luxforge_core::catalog_types::jobs::INDEX_REFRESH.activity;

/// Long-running work's own state in the editor: the watch on the board, what it read, and when.
#[derive(Debug, Default)]
pub(crate) struct LongWork {
    watch: Option<ActivityWatch>,
    pub(crate) state: LongWorkState,
    read_at: Option<Instant>,
    /// A wake came inside the throttle's interval: the next tick reads.
    due: bool,
    /// Shared native gate; lifecycle wakes stay live while display timers/progress are paused.
    presentation_visible: bool,
    /// The recent entries the last read listed, so a read tells which jobs just ended.
    ended: Vec<u64>,
    /// The index lane's listings (`index.refresh` jobs) the last read listed running, so a read
    /// tells whether one began or ended.
    listings: Vec<u64>,
    /// Reads long work took and wakes since launch, for evidence: a run proves long work asleep by
    /// these staying put while nothing runs. The Performance section's reads of the board are its
    /// own and not counted here.
    pub(crate) reads: u64,
    pub(crate) wakes: u64,
    /// The jobs a Cancel was sent for, newest last.
    pub(crate) cancels: Vec<String>,
}

/// The timers long work holds now.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Timers {
    /// A read is due after a wake inside the throttle's interval.
    pub(crate) throttle: bool,
    /// A catalog job runs.
    pub(crate) refresh: bool,
}

fn signal() -> &'static Signal<Message> {
    static SIGNAL: OnceLock<Signal<Message>> = OnceLock::new();
    SIGNAL.get_or_init(|| Signal::new(|| Message::LongWork(LongWorkMessage::Woken)))
}

/// Presentation can stop following board progress while these authoritative interests keep
/// final results (and Locate's business-relevant partial answers) alive.
#[derive(Clone, Debug, Hash)]
enum Tracked {
    Reading(String),
    Adding(String),
    Search(String),
    Locate(String),
    Batch(String),
}

fn job_reader(reader: &super::job_reads::Reader<Tracked>) -> BoxStream<'static, Message> {
    let reader = (
        reader.owner.clone(),
        reader.client,
        reader.identity.clone(),
        reader.presentation_visible,
        super::job_reads::Watch::<Value>::default(),
        None,
        false,
    );
    stream::unfold(
        reader,
        |(owner, client, tracked, visible, mut watch, mut after, done)| async move {
            if done {
                return None;
            }
            loop {
                let job = match &tracked {
                    Tracked::Reading(job)
                    | Tracked::Adding(job)
                    | Tracked::Search(job)
                    | Tracked::Locate(job)
                    | Tracked::Batch(job) => job,
                };
                let result = super::job_reads::wait(&owner, client, job, after)
                    .await
                    .map(|(change, record)| {
                        after = Some(change);
                        record
                    });
                let finished = result.as_ref().map_or(true, |record| {
                    !matches!(record["status"].as_str(), Some("queued" | "running"))
                });
                if matches!(&tracked, Tracked::Search(_))
                    && watch.observe_filtered(
                        &result,
                        |_| finished,
                        |one, two| {
                            if visible {
                                one == two
                            } else {
                                super::job_reads::same_without_progress(one, two)
                            }
                        },
                    ) == super::job_reads::Verdict::Skip
                {
                    continue;
                }
                let message = match &tracked {
                    Tracked::Reading(job) if finished => {
                        Message::Select(super::message::select::SelectMessage::ReadWatched {
                            job: job.clone(),
                            result,
                        })
                    }
                    Tracked::Adding(job) if finished => {
                        Message::Select(super::message::select::SelectMessage::AddListed {
                            job: job.clone(),
                            result,
                        })
                    }
                    Tracked::Search(job) => {
                        Message::Select(super::message::select::SelectMessage::Missing(
                            super::message::select_missing::MissingMessage::Polled {
                                search: Some((job.clone(), result)),
                                locate: None,
                            },
                        ))
                    }
                    Tracked::Locate(job) if finished => {
                        Message::Select(super::message::select::SelectMessage::Missing(
                            super::message::select_missing::MissingMessage::Polled {
                                search: None,
                                locate: Some((job.clone(), result)),
                            },
                        ))
                    }
                    Tracked::Batch(job) if finished => {
                        Message::Select(super::message::select::SelectMessage::Catalog(
                            super::message::select_catalog::CatalogMessage::BatchRead {
                                job: job.clone(),
                                result,
                            },
                        ))
                    }
                    _ => continue,
                };
                return Some((
                    message,
                    (owner, client, tracked, visible, watch, after, finished),
                ));
            }
        },
    )
    .fuse()
    .boxed()
}

impl LongWork {
    /// Watch `owner`'s board for catalog work and read it once, which arms the watch. A board that
    /// refuses the watch leaves long work showing nothing, which is reported and never guessed.
    pub(crate) fn watching(owner: &OwnerHandle, presentation_visible: bool) -> Self {
        let watch = owner
            .activity()
            .watch(followed, Arc::new(|| signal().post()))
            .map_err(|error| eprintln!("long-running work is not shown: {error}"))
            .ok();
        let mut work = Self {
            watch,
            presentation_visible,
            ..Self::default()
        };
        if let Some(watch) = &work.watch {
            let board = watch.read_with_progress(presentation_visible);
            work.ended = board.recent.iter().map(|job| job.entry.id).collect();
            work.state.observe(board);
            work.read_at = Some(Instant::now());
        }
        work
    }

    /// The timers that exist now: none while nothing runs and no read is due.
    pub(crate) fn timers(&self) -> Timers {
        Timers {
            throttle: self.presentation_visible && self.due,
            refresh: self.presentation_visible && self.state.running().next().is_some(),
        }
    }
}

/// `job.cancel` for one job: the request a Performance row's Cancel and the sheet's Cancel send.
pub(crate) fn cancel_now(
    owner: &OwnerHandle,
    client: ClientId,
    job_id: &str,
) -> Result<(), String> {
    call(
        owner,
        client,
        luxforge_core::jobs::JOB_CANCEL,
        json!({ "job_id": job_id }),
    )
    .map(|_| ())
}

/// What one read of the board found beyond the model's own state.
struct Read {
    /// Followed jobs that ended since the last read.
    ended: Vec<RecentActivity>,
    /// A listing of the index lane's began or ended since the last read.
    listings: bool,
}

impl Editor {
    /// Reconcile once across a native visibility transition. Hidden watches continue to hear
    /// lifecycle changes, with no display throttle or elapsed-time refresh timer.
    pub(crate) fn long_work_visibility_changed(&mut self) -> Task<Message> {
        self.long_work.presentation_visible = self.visibility.sampling_allowed();
        self.long_work.due = false;
        self.read_board()
    }

    /// One long-running-work message.
    pub(crate) fn long_work_update(&mut self, message: LongWorkMessage) -> Task<Message> {
        match message {
            LongWorkMessage::OpenPerformance => return self.open_performance(),
            LongWorkMessage::Cancel { job_id } => return self.cancel_job(job_id),
            LongWorkMessage::ContinueInBackground => {
                let work = &mut self.long_work.state;
                if let Some(waiting) = &work.waiting {
                    work.background = Some(waiting.job_id.clone());
                }
            }
            LongWorkMessage::Woken => {
                self.long_work.wakes += 1;
                if !self.visibility.sampling_allowed() {
                    return self.read_board();
                }
                let throttled = self
                    .long_work
                    .read_at
                    .is_some_and(|at| at.elapsed() < MIN_INTERVAL);
                if throttled {
                    self.long_work.due = true;
                } else {
                    return self.read_board();
                }
            }
            LongWorkMessage::Tick => {
                if !self.visibility.sampling_allowed() {
                    return Task::none();
                }
                // A refresh right after a read has nothing to add, and would lift the reads past
                // the throttle's rate; a due read is always taken.
                let recent = self
                    .long_work
                    .read_at
                    .is_some_and(|at| at.elapsed() < MIN_INTERVAL);
                if self.long_work.due || !recent {
                    return self.read_board();
                }
            }
            LongWorkMessage::Cancelled { job_id, result } => {
                if let Err(error) = result {
                    self.event(
                        "long_work_cancel_refused",
                        || json!({"job_id": job_id, "reason": error}),
                    );
                    self.status.text = format!("Could not cancel: {error}");
                }
            }
            LongWorkMessage::Ended { job, result } => {
                // A card's or folder's reading for Select, which words its own cancellation or
                // failure, whichever of the two hears of the end first.
                let reading = job.entry.job_id.as_deref().is_some_and(|ended| {
                    let reading = self
                        .select
                        .reading
                        .as_ref()
                        .and_then(|at| at.job.as_deref());
                    reading == Some(ended) || self.select.worded.as_deref() == Some(ended)
                });
                let worded_by_select =
                    reading && job.outcome != luxforge_core::activity::Outcome::Completed;
                let record = result.ok();
                if worded_by_select {
                    return Task::none();
                }
                if let Some(sentence) =
                    finished_sentence(&job, record.as_ref(), self.select.state.home.as_deref())
                {
                    self.status.text = sentence;
                }
            }
        }
        Task::none()
    }

    /// Read the board as long work: a wake, a tick, or the waiting view just named.
    fn read_board(&mut self) -> Task<Message> {
        if self.long_work.watch.is_some() {
            self.long_work.reads += 1;
        }
        self.take_in_board()
    }

    /// The Performance section's read of the board at its sampler's tick: the board its job rows
    /// show is the one the status bar's job comes from, so the section reads it here, through long
    /// work's watch, and what it finds is taken in exactly as long work's own reads are. With no
    /// watch there is no board to read, and the section lists no work, as long work shows none.
    pub(crate) fn section_reads_board(&mut self) -> Task<Message> {
        self.take_in_board()
    }

    /// Read the board and take in what changed: the model's state and each job's rate, the sentence
    /// of each job that just ended, and the view waiting on a job.
    fn take_in_board(&mut self) -> Task<Message> {
        let Some(read) = self.take_board() else {
            return Task::none();
        };
        let waiting = self
            .long_work
            .state
            .waiting
            .as_ref()
            .map(|waiting| waiting.job_id.clone());
        let mut tasks: Vec<Task<Message>> = read
            .ended
            .into_iter()
            .filter(|job| {
                job.duration_ms >= FINISHED_SENTENCE_MS
                    && job.entry.job_id.is_some()
                    && job.entry.job_id != waiting
            })
            .map(|job| {
                let (owner, client) = (self.owner.clone(), self.client);
                let job_id = job.entry.job_id.clone().unwrap_or_default();
                let job = Box::new(job);
                owner_task(
                    move || job_now(&owner, client, &job_id),
                    move |result| Message::LongWork(LongWorkMessage::Ended { job, result }),
                )
            })
            .collect();
        tasks.push(self.reading_followed());
        if read.listings {
            tasks.push(self.select_listings_changed());
        }
        Task::batch(tasks)
    }

    /// One read of the board, re-arming the watch.
    fn take_board(&mut self) -> Option<Read> {
        let work = &mut self.long_work;
        let board = work
            .watch
            .as_ref()?
            .read_with_progress(self.visibility.sampling_allowed());
        work.read_at = Some(Instant::now());
        work.due = false;
        let ended: Vec<RecentActivity> = board
            .recent
            .iter()
            .filter(|job| followed(&job.entry) && !work.ended.contains(&job.entry.id))
            .cloned()
            .collect();
        work.ended = board.recent.iter().map(|job| job.entry.id).collect();
        let listings: Vec<u64> = board
            .active
            .iter()
            .filter(|job| job.entry.kind == LISTING)
            .map(|job| job.entry.id)
            .collect();
        let listed = listings != work.listings || ended.iter().any(|job| job.entry.kind == LISTING);
        work.listings = listings;
        work.state.observe(board);
        Some(Read {
            ended,
            listings: listed,
        })
    }

    /// The status bar's job was pressed: the Performance section, expanded, in its panel on screen.
    fn open_performance(&mut self) -> Task<Message> {
        self.performance.expanded = true;
        if self.select_shown() {
            self.select.state.sources_panel = true;
            return Task::none();
        }
        if self.session.workspace.state_panel {
            return Task::none();
        }
        self.dispatch(Message::View(ViewMessage::TogglePanel(Panel::State)))
    }

    /// `job.cancel` for `job_id`, as its Performance row or the progress sheet sends it. What
    /// happened reaches the screen through the board: the job ends cancelled.
    fn cancel_job(&mut self, job_id: String) -> Task<Message> {
        self.event("long_work_cancel", || json!({"job_id": job_id}));
        let cancels = &mut self.long_work.cancels;
        cancels.push(job_id.clone());
        if cancels.len() > CANCELS_KEPT {
            cancels.remove(0);
        }
        let (owner, client) = (self.owner.clone(), self.client);
        let asked = job_id.clone();
        owner_task(
            move || cancel_now(&owner, client, &asked),
            move |result| Message::LongWork(LongWorkMessage::Cancelled { job_id, result }),
        )
    }

    /// Long-running work as a captured frame records it: the model the status bar and the sheet
    /// drew, the catalog work on the board it came from, and the reads, wakes and timers behind it.
    pub(crate) fn long_work_summary(&self) -> Value {
        let work = &self.long_work;
        let model = &self.workspace.long_work;
        let board = work.state.board.as_ref();
        json!({
            "busiest": model.busiest.as_ref().map(|job| json!({
                "job_id": job.job_id,
                "label": job.label,
                "fraction": job.fraction,
                "jobs": job.jobs,
            })),
            "sheet": model.sheet.as_ref().map(|sheet| json!({
                "job_id": sheet.job_id,
                "card": sheet.card,
                "title": sheet.title,
                "note": sheet.note,
                "count": sheet.count,
                "fraction": sheet.fraction,
                "estimate": sheet.estimate,
            })),
            "waiting": work.state.waiting.as_ref().map(|waiting| json!({
                "job_id": waiting.job_id,
                "name": waiting.name,
                "card": waiting.card,
            })),
            "background": work.state.background,
            "running": board.map(|board| board.active.iter().filter(|job| followed(&job.entry)).collect::<Vec<_>>()),
            "recent": board.map(|board| board.recent.iter().filter(|job| followed(&job.entry)).collect::<Vec<_>>()),
            "sequence": board.map(|board| board.sequence),
            "reads": work.reads,
            "wakes": work.wakes,
            "timers": {"throttle": work.timers().throttle, "refresh": work.timers().refresh},
            "cancels": work.cancels,
        })
    }
}

/// After every message: follow the Select view waiting on a job, reading the board at once when it
/// names a new one. An evidence run also hears that what long work shows may have changed.
pub(super) fn after_message(editor: &mut Editor, _: &Before) -> Task<Message> {
    let waiting = editor.select.reading.as_ref().and_then(|reading| {
        Some(Waiting {
            job_id: reading.job.clone()?,
            name: reading.source.sheet_name(),
            card: matches!(reading.source, ReadSource::Card { .. }),
        })
    });
    let task = if waiting != editor.long_work.state.waiting {
        let named = waiting.as_ref().is_some_and(|now| {
            editor
                .long_work
                .state
                .waiting
                .as_ref()
                .is_none_or(|before| before.job_id != now.job_id)
        });
        editor.long_work.state.waiting = waiting;
        if named {
            editor.read_board()
        } else {
            Task::none()
        }
    } else {
        Task::none()
    };
    if editor.evidence.is_some() {
        editor.outcome(Outcome::LongWorkShown);
    }
    task
}

/// The signal the watch posts, which carries a wake into the event loop — a blocked stream, woken
/// only by the board — and the two timers, each only while it has something to do ([`Timers`]).
pub(super) fn subscription(editor: &Editor) -> Subscription<Message> {
    let work = &editor.long_work;
    let timers = work.timers();
    let mut subscriptions = Vec::new();
    if work.watch.is_some() {
        subscriptions.push(Subscription::run(|| signal().stream()));
    }
    if timers.throttle {
        subscriptions.push(
            iced::time::every(MIN_INTERVAL).map(|_| Message::LongWork(LongWorkMessage::Tick)),
        );
    }
    if timers.refresh {
        subscriptions.push(
            iced::time::every(REFRESH_INTERVAL).map(|_| Message::LongWork(LongWorkMessage::Tick)),
        );
    }
    let reading = editor
        .select
        .reading
        .as_ref()
        .and_then(|reading| reading.job.clone())
        .map(Tracked::Reading);
    let adding = editor
        .select
        .adding
        .as_ref()
        .and_then(|adding| adding.job.clone())
        .map(Tracked::Adding);
    let (search, locate) = editor.select.state.missing.running_jobs();
    let batch = editor
        .select
        .state
        .catalog
        .running()
        .map(|batch| Tracked::Batch(batch.job.clone()));
    for identity in [
        reading,
        adding,
        search.map(Tracked::Search),
        locate.map(Tracked::Locate),
        batch,
    ]
    .into_iter()
    .flatten()
    {
        subscriptions.push(Subscription::run_with(
            super::job_reads::Reader {
                identity,
                owner: editor.owner.clone(),
                client: editor.client,
                presentation_visible: editor.visibility.sampling_allowed(),
            },
            job_reader,
        ));
    }
    Subscription::batch(subscriptions)
}

#[cfg(test)]
mod tests;
