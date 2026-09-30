//! Lane A's calls that wait on the index lane's query and survey threads (`crate::index::query`),
//! parked on the owner as `events.wait` is: the owner never touches the file system beyond the
//! platform's mount table, which waits on none.
//!
//! - **A question for the query thread.** A handler that must stat, canonicalize or list a path
//!   hands the query thread an [`Ask`] ([`ask`]): the thread runs it and posts back a [`Resume`],
//!   which the owner runs to answer the call, with the catalog as it is then. Every such call waits
//!   for an owner step afterwards (a library change, a job, the index's rows), so the thread never
//!   answers a client itself: one path, whose answer carries the owner's event sequence, records
//!   its retry, and is dropped with the client that left.
//! - **One at a time, bounded.** The thread runs one question at a time, in the order asked; at most
//!   [`MAX_WAITING_QUERIES`] calls wait behind it, and one more is refused with `resource-limit`.
//!   A question stuck on a volume that does not answer holds that thread and the calls behind it,
//!   never the owner: they wait while there is room and are refused once there is none, and a
//!   client that leaves takes its waiting calls with it.
//! - **The survey.** `volume.list`, `card.list` and `index.folders` answer at once from what the
//!   survey thread last learned ([`crate::index::survey::Known`]) against the mount table read
//!   now, and ask for a new survey when that knowledge has already answered a call
//!   ([`learned`]); only the first calls, before any survey has posted, wait for it
//!   ([`wait_for_survey`]), and are answered again as if just asked once it has. [`request_survey`]
//!   is what the mount notifications will call.
//! - **The index.** Lane A's reads use the service's index only once it is open: the survey opens
//!   an index that is there, and the index lane hands over the one it creates, so neither opening
//!   nor recreating it happens on the owner.
use super::{super::requests::RequestKey, FilesLane, FilesMessage, indexed_folders};
use crate::{
    Error,
    api::{
        ApiResponse,
        owner::{ClientId, Owner, OwnerCall, catalog::CatalogMessage},
    },
    editor::now_ms,
    index::{
        IndexDb,
        query::{Job, Worker},
        survey::{self, Known, Mounts, Survey},
    },
};
use serde_json::Value;
use std::{
    cell::RefMut,
    collections::VecDeque,
    panic::{AssertUnwindSafe, catch_unwind},
};

/// Calls that may wait on lane A's threads at once, behind the question being answered; past it a
/// call is refused with `resource-limit`. Each has a client blocked on it, and every client
/// connection carries one call at a time, so this is twice the live clients' limit: room for the
/// desktop's calls beside them.
pub(crate) const MAX_WAITING_QUERIES: usize = 16;

/// What the query thread runs for one call: every file system read the call needs, answering what
/// the owner then does with the call.
pub(in crate::api::owner) type Ask = Box<dyn FnOnce() -> Resume + Send>;

/// The owner's half of a call the query thread answered: it runs on the owner with what the thread
/// found and answers the call.
pub(in crate::api::owner) type Resume = Box<dyn FnOnce(&mut Owner) -> Result<Value, Error> + Send>;

/// What a lane A handler asks for when it cannot answer yet. The handler leaves it in
/// `Owner::deferred` and answers nothing; the owner hands it the call with its reply ([`defer`]).
pub(in crate::api::owner) enum Deferred {
    /// Answer once the query thread has run `ask`. `key` is the call's retry key, when the owner
    /// answers its retries: its first answer is recorded when it is given.
    Query { ask: Ask, key: Option<RequestKey> },
    /// Answer the call again, as if just asked, once the first survey has posted.
    Survey,
}

/// Hand `ask` to the query thread and answer the call when it has run: the handler's answer.
pub(super) fn ask(
    owner: &mut Owner,
    ask: impl FnOnce() -> Resume + Send + 'static,
) -> Result<Value, Error> {
    owner.deferred = Some(Deferred::Query {
        ask: Box::new(ask),
        key: None,
    });
    Ok(Value::Null)
}

/// Whether the surveys have learned what this call reads; when they have and a call has already
/// read it, a new survey is asked for, so what the next call reads is current (until the mount
/// notifications ask for one when something changes).
pub(super) fn learned(owner: &mut Owner) -> bool {
    let surveys = &mut owner.catalog.files.surveys;
    if !surveys.surveyed {
        return false;
    }
    if std::mem::replace(&mut surveys.consumed, true) {
        request_survey(owner);
    }
    true
}

/// Wait for the first survey, which is started if none is running: the handler's answer.
pub(super) fn wait_for_survey(owner: &mut Owner) -> Result<Value, Error> {
    let surveys = &mut owner.catalog.files.surveys;
    match surveys.running {
        // The survey running is of this generation: its first post answers the call.
        Some(generation) if generation == surveys.generation => {}
        Some(_) => surveys.again = true,
        None => start_survey(owner),
    }
    owner.deferred = Some(Deferred::Survey);
    Ok(Value::Null)
}

/// Ask for a survey: now when none is running, else once the running one ends. The mount
/// notifications call this when a volume is mounted or taken out.
pub(super) fn request_survey(owner: &mut Owner) {
    if owner.catalog.files.surveys.running.is_some() {
        owner.catalog.files.surveys.again = true;
    } else {
        start_survey(owner);
    }
}

/// The volumes mounted now, as the surveys learned them: what is in the platform's table read now
/// ([`Known::now`]). Empty before the first survey has posted.
pub(super) fn mounted(owner: &Owner) -> Mounts<'_> {
    let files = &owner.catalog.files;
    files.surveys.known.now(&files.mounts.list())
}

/// The catalog's index for lane A's reads on the owner, never opened here: the service's open
/// index; none when the last survey found no index; or why the survey could not open it.
pub(super) fn index(owner: &Owner) -> Result<Option<RefMut<'_, IndexDb>>, Error> {
    if let Some(index) = owner.service.index_open() {
        return Ok(Some(index));
    }
    match &owner.catalog.files.surveys.index_error {
        Some(error) => Err(error.clone()),
        None => Ok(None),
    }
}

/// The query thread, the question it is answering and the calls waiting for it.
#[derive(Default)]
pub(super) struct Queries {
    worker: Option<Worker>,
    waiting: VecDeque<Queued>,
    /// The call whose question the thread is answering; `None` inside once its client has left.
    running: Option<Option<Parked>>,
    /// Held before each question from the next one on, as a volume that does not answer would
    /// hold it, for a test that acts meanwhile.
    #[cfg(test)]
    pub(super) hold: Option<std::sync::Arc<luxforge_testbase::Gate>>,
}

/// The survey thread, what the surveys have learned, and the calls waiting for the first.
#[derive(Default)]
pub(super) struct Surveys {
    worker: Option<Worker>,
    /// The generation of the survey running, if one is.
    running: Option<u64>,
    /// Another survey was asked for while one ran: it starts once that one ends.
    again: bool,
    /// Which mount source the surveys read; a test's new source starts a new generation, whose
    /// first survey the calls wait for.
    generation: u64,
    /// Whether a survey of this generation has posted.
    surveyed: bool,
    /// What this generation's surveys have learned.
    known: Known,
    /// Whether a call has read `known` since it was last posted.
    consumed: bool,
    waiting: Vec<OwnerCall>,
    /// Why the last survey could not open the index, when it could not.
    index_error: Option<Error>,
}

/// A call waiting for its question to be answered.
struct Parked {
    client: ClientId,
    id: String,
    key: Option<RequestKey>,
    response: std::sync::mpsc::SyncSender<ApiResponse>,
}

struct Queued {
    parked: Parked,
    ask: Ask,
}

/// One post of a survey.
pub(in crate::api::owner) struct SurveyPost {
    generation: u64,
    /// What it learned; none when the survey failed before learning anything.
    survey: Option<Survey>,
    /// The index it opened for the owner, none when there is none, or why it could not; absent
    /// when the owner had one open.
    index: Option<Result<Option<IndexDb>, Error>>,
    last: bool,
}

/// `Owner::call` hands a deferred call here with its reply: it waits for the query thread or the
/// survey, or is refused at once when [`MAX_WAITING_QUERIES`] already wait.
pub(in crate::api::owner) fn defer(owner: &mut Owner, call: OwnerCall, deferred: Deferred) {
    let files = &mut owner.catalog.files;
    if files.queries.waiting.len() + files.surveys.waiting.len() >= MAX_WAITING_QUERIES {
        let error = Error::resource_limit(format!(
            "{MAX_WAITING_QUERIES} calls already wait for the index lane to read the disk; retry after one is answered"
        ));
        let _ = call.response.try_send(ApiResponse::failure(
            call.request.id,
            owner.log.sequence,
            error,
        ));
        return;
    }
    match deferred {
        Deferred::Survey => files.surveys.waiting.push(call),
        Deferred::Query { ask, key } => {
            files.queries.waiting.push_back(Queued {
                parked: Parked {
                    client: call.client,
                    id: call.request.id,
                    key,
                    response: call.response,
                },
                ask,
            });
            dispatch(owner);
        }
    }
}

/// Hand the query thread the next question when it is idle, starting it on first use.
fn dispatch(owner: &mut Owner) {
    loop {
        let files = &mut owner.catalog.files;
        if files.queries.running.is_some() {
            return;
        }
        let Some(Queued { parked, ask }) = files.queries.waiting.pop_front() else {
            return;
        };
        let job = question(files, ask);
        let sent = match &files.queries.worker {
            Some(worker) => worker.send(job),
            None => Worker::start("luxforge-index-query")
                .and_then(|worker| worker.send(job).map(|()| worker))
                .map(|worker| files.queries.worker = Some(worker)),
        };
        match sent {
            Ok(()) => {
                files.queries.running = Some(Some(parked));
                return;
            }
            Err(error) => {
                files.queries.worker = None;
                answer(owner, parked, Err(error));
            }
        }
    }
}

/// The job that answers one question on the query thread and posts its [`Resume`] back. A question
/// that panics answers `internal`.
fn question(files: &FilesLane, ask: Ask) -> Job {
    let poster = files.poster.clone();
    #[cfg(test)]
    let hold = files.queries.hold.clone();
    Box::new(move || {
        #[cfg(test)]
        if let Some(hold) = &hold {
            hold.pass();
        }
        let resume = catch_unwind(AssertUnwindSafe(ask)).unwrap_or_else(|_| {
            Box::new(|_| Err(Error::internal("reading the disk failed unexpectedly")))
        });
        poster.post(CatalogMessage::Files(FilesMessage::Answered(resume)));
    })
}

/// The query thread answered the running question: resume its call on the owner and answer it,
/// unless its client has left, then hand the thread the next question.
pub(super) fn answered(owner: &mut Owner, resume: Resume) {
    if let Some(Some(parked)) = owner.catalog.files.queries.running.take() {
        let result = catch_unwind(AssertUnwindSafe(|| resume(owner)))
            .unwrap_or_else(|_| Err(Error::internal("the call could not be answered")));
        answer(owner, parked, result);
    }
    dispatch(owner);
}

/// Answer a parked call as the owner answers any call: after recording what it announced, with
/// the event sequence now, its first answer kept for its retries, and waking the other clients
/// watching the log.
fn answer(owner: &mut Owner, parked: Parked, result: Result<Value, Error>) {
    let Parked {
        client,
        id,
        key,
        response,
    } = parked;
    owner.record_announced();
    let reply = match result {
        Ok(mut value) => {
            if let Some(key) = key {
                owner.requests.record(key, &mut value);
            }
            ApiResponse::success(id, owner.log.sequence, value)
        }
        Err(error) => ApiResponse::failure(id, owner.log.sequence, error),
    };
    // Each call's reply channel has room for its one answer, so this never waits; a caller that
    // has gone has nobody to answer.
    let _ = response.try_send(reply);
    owner.notify_watchers(Some(client));
}

/// Start a survey on the survey thread, starting the thread on first use. It reads the mount
/// table and the indexed folders as they are now, and opens the index when the owner has none
/// open. A survey that cannot start refuses the calls waiting for it.
fn start_survey(owner: &mut Owner) {
    let folders: Vec<_> = indexed_folders(owner)
        .unwrap_or_default()
        .into_iter()
        .map(|folder| (folder.path, folder.volume_id))
        .collect();
    let index = owner.service.index_open().is_none().then(|| {
        (
            owner.service.index_dir().to_path_buf(),
            owner.service.catalog_id().to_owned(),
        )
    });
    let files = &mut owner.catalog.files;
    let generation = files.surveys.generation;
    let (mounts, poster) = (files.mounts.clone(), files.poster.clone());
    let job: Job = Box::new(move || {
        let post = |post: SurveyPost| {
            poster.post(CatalogMessage::Files(FilesMessage::Surveyed(Box::new(
                post,
            ))));
        };
        let mut index = index.map(|(dir, catalog_id)| IndexDb::open_existing(&dir, &catalog_id));
        let surveyed = catch_unwind(AssertUnwindSafe(|| {
            survey::survey(mounts.list(), &folders, now_ms(), |survey, last| {
                post(SurveyPost {
                    generation,
                    survey: Some(survey),
                    index: index.take(),
                    last,
                });
            });
        }));
        // A survey that failed still ends, so the next can start.
        if surveyed.is_err() {
            post(SurveyPost {
                generation,
                survey: None,
                index: index.take(),
                last: true,
            });
        }
    });
    let sent = match &files.surveys.worker {
        Some(worker) => worker.send(job),
        None => Worker::start("luxforge-index-survey")
            .and_then(|worker| worker.send(job).map(|()| worker))
            .map(|worker| files.surveys.worker = Some(worker)),
    };
    match sent {
        Ok(()) => files.surveys.running = Some(generation),
        Err(error) => {
            files.surveys.worker = None;
            for call in std::mem::take(&mut files.surveys.waiting) {
                refuse(owner.log.sequence, call, error.clone());
            }
        }
    }
}

/// One post of a survey: take in what it learned when it is of this generation, adopt the index it
/// opened, answer again the calls that waited for it, and start the survey asked for meanwhile
/// once it ends.
pub(super) fn surveyed(owner: &mut Owner, post: SurveyPost) {
    let SurveyPost {
        generation,
        survey,
        index,
        last,
    } = post;
    match index {
        Some(Ok(Some(index))) => {
            owner.service.adopt_index(index);
            owner.catalog.files.surveys.index_error = None;
        }
        Some(Ok(None)) => owner.catalog.files.surveys.index_error = None,
        Some(Err(error)) => owner.catalog.files.surveys.index_error = Some(error),
        None => {}
    }
    let surveys = &mut owner.catalog.files.surveys;
    let current = generation == surveys.generation;
    if current && let Some(survey) = survey {
        surveys.known.merge(survey);
        surveys.surveyed = true;
        surveys.consumed = false;
    }
    if last {
        surveys.running = None;
    }
    if current {
        let waiting = std::mem::take(&mut surveys.waiting);
        if surveys.surveyed {
            for call in waiting {
                again(owner, call);
            }
        } else if last {
            let error = Error::internal("the volumes could not be surveyed");
            for call in waiting {
                refuse(owner.log.sequence, call, error.clone());
            }
        } else {
            owner.catalog.files.surveys.waiting = waiting;
        }
    }
    let surveys = &mut owner.catalog.files.surveys;
    if last && std::mem::take(&mut surveys.again) {
        start_survey(owner);
    }
}

/// Answer a call that waited for the survey as if it had just been asked. A panic while answering
/// it answers it `internal`, as the owner does for any call.
fn again(owner: &mut Owner, call: OwnerCall) {
    let (id, response) = (call.request.id.clone(), call.response.clone());
    if catch_unwind(AssertUnwindSafe(|| owner.call(call))).is_err() {
        owner.deferred = None;
        owner.record_announced();
        let _ = response.try_send(ApiResponse::failure(
            id,
            owner.log.sequence,
            Error::internal("the catalog owner failed while serving this message"),
        ));
    }
}

/// Refuse a call that is still waiting, with `error`.
fn refuse(sequence: u64, call: OwnerCall, error: Error) {
    let _ = call
        .response
        .try_send(ApiResponse::failure(call.request.id, sequence, error));
}

impl FilesLane {
    /// Forget a client that has gone: its calls waiting on the threads are dropped unanswered, and
    /// the answer to the question the query thread is answering for it will go nowhere.
    pub(super) fn forget_waiting(&mut self, client: ClientId) {
        self.queries
            .waiting
            .retain(|queued| queued.parked.client != client);
        if let Some(running) = &mut self.queries.running
            && running
                .as_ref()
                .is_some_and(|parked| parked.client == client)
        {
            *running = None;
        }
        self.surveys.waiting.retain(|call| call.client != client);
    }

    /// A test's new mount source: what was learned from the old one is forgotten, and the next
    /// calls wait for a survey of the new one.
    #[cfg(test)]
    pub(super) fn new_mount_source(&mut self) {
        let surveys = &mut self.surveys;
        surveys.generation += 1;
        surveys.surveyed = false;
        surveys.known = Known::default();
    }

    /// How many calls wait behind the question the query thread is answering.
    #[cfg(test)]
    pub(super) fn queries_waiting(&self) -> usize {
        self.queries.waiting.len()
    }

    /// Stop both threads as the owner stops. The waiting calls are dropped unanswered, as every
    /// call is when the owner stops. An idle thread is joined; one still answering a question is
    /// not waited for, since the volume it reads may never answer ([`Worker::stop`]).
    pub(super) fn stop_threads(&mut self) {
        self.queries.waiting.clear();
        self.surveys.waiting.clear();
        if let Some(worker) = self.queries.worker.take() {
            worker.stop(self.queries.running.is_some());
        }
        if let Some(worker) = self.surveys.worker.take() {
            worker.stop(self.surveys.running.is_some());
        }
    }
}
