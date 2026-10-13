//! The merge preview's sessions and commands on the owner: `merge.preview`,
//! `merge.preview-update`, `merge.preview-close`, and `merge.create`'s `preview_id`
//! (`docs/design/panorama-merge.md`, "The merge preview"; `docs/design/hdr-merge.md`, "The merge
//! preview"). The work is `crate::merge::preview`.
//!
//! - **A session** is one client's open preview: its inputs as the preview read them, the latest
//!   options asked for (its revision), and, once read, the bracket's spool and proxies and the
//!   latest alignment, in a slot the jobs that continue it read ([`Retained`]). A client has at
//!   most one; opening another closes the first.
//! - **Preparing** runs on the merge lane, which it shares with merges (one running, one waiting,
//!   `resource-limit` beyond), and spools under `merge-previews/<preview_id>/` beside the merges'
//!   folders. **Updates** run on the merge preview lane, reading no frame, so they never wait
//!   behind a merge; an update asked for while the preview is still preparing waits behind it on
//!   the merge lane instead, outside the bound. A newer update cancels a stale one.
//! - **Bounds.** A ready preview's spool counts against its kind's temporary ceiling, beside any
//!   merge's spool and every other ready preview's ([`Sessions::held`]), until it is merged, closed,
//!   idle for [`IDLE`] (the owner's own wake-up, [`Sessions::next_idle`]), or its client
//!   disconnects. Every merge or preview job removes, as it starts, the preview folders and
//!   pictures no open session keeps, which only a stopped process leaves.
//! - **Merging** a preview (`merge.create` with `preview_id`) takes its bracket over: the merge's
//!   options must be the latest revision's and its inputs the preview's (`conflict` otherwise, as
//!   for a preview that is not open); a preview still preparing is continued behind its own job,
//!   never restarted; the session is merging, then gone, with its pictures, when the merge ends.
//!
//! A preview publishes nothing and is not a library change.
use super::{
    super::{
        Call, ClientId, Owner,
        library::{JobContext, LibraryLane, Task},
    },
    Named, header_check, inputs, options, planned_frames, requested, signed,
};
use crate::{
    Error, JobId, PREVIEWS_DIR,
    api::methods::value,
    artifacts::{RootState, root_state},
    catalog_types::{
        MergeKind, MergeOptions, MergeParameters, MergePreview, MergePreviewId,
        api::{MergePreviewClose, MergePreviewParams, MergePreviewUpdate},
        jobs::MERGE_PREVIEW,
    },
    jobs::JobKind,
    merge::{
        job::{self, Analysis, JobInput, Prepared},
        panorama::{
            job::{PanoramaAnalysis, PanoramaPrepared, spool_bytes},
            preview::PanoramaUpdate,
        },
        preview::{self, Drawn, Pictures},
    },
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// How long a ready preview nobody touches stays open.
pub(in crate::api::owner) const IDLE: Duration = Duration::from_secs(30 * 60);
/// Updates that may wait on the merge preview lane behind the running one: one a client, as each
/// client's newer update cancels its stale one.
pub(in crate::api::owner) const PREVIEW_QUEUE: usize = 16;
/// The folder under the artifact root holding each open preview's spool.
const PREVIEWS: &str = "merge-previews";
/// The folder in the preview cache holding the previews' pictures.
const PICTURES: &str = "merges";

/// A preview's frames once read, and its latest alignment or registration: what the jobs
/// continuing it read when they run, of its kind.
#[derive(Clone)]
pub(super) enum Retained {
    Hdr {
        prepared: Arc<Prepared>,
        analysis: Option<Arc<Analysis>>,
    },
    Panorama {
        prepared: Arc<PanoramaPrepared>,
        analysis: Option<Arc<PanoramaAnalysis>>,
    },
}

/// Where a session's bracket is kept for the jobs that continue it.
pub(super) type Slot = Arc<Mutex<Option<Retained>>>;

/// The latest revision a session answered: whether it was refused, and how.
struct Answered {
    revision: u32,
    refusal: Option<Value>,
}

/// One client's open preview: see the module documentation.
struct Session {
    client: ClientId,
    kind: MergeKind,
    named: Vec<Named>,
    /// The inputs as the preview read them, their signatures then.
    inputs: Vec<JobInput>,
    /// The latest revision asked for, its options as resolved and its parameters.
    revision: u32,
    parameters: MergeParameters,
    answered: Option<Answered>,
    slot: Slot,
    /// Reading refused the bracket: there are no frames to draw or merge.
    unread: bool,
    preparing: Option<JobId>,
    update: Option<JobId>,
    merging: Option<JobId>,
    /// The temporary bytes its spool counts against the ceiling while it is ready.
    held_bytes: u64,
    last_used: Instant,
    folder: PathBuf,
    pictures: Pictures,
}

/// The owner's merge preview sessions, beside the merge lane: see the module documentation.
pub(in crate::api::owner) struct Sessions {
    open: HashMap<MergePreviewId, Session>,
    /// The previews whose folders and pictures a job's sweep keeps: every open session's, shared
    /// with the workers.
    kept: Arc<Mutex<HashSet<String>>>,
    /// The temporary bytes ready previews keep spooled, which every merge and preview of the lane
    /// counts against its ceiling.
    pub(super) held: Arc<AtomicU64>,
    /// How long a ready preview stays open untouched: [`IDLE`], which a test shortens.
    pub(in crate::api::owner) idle: Duration,
}

impl Default for Sessions {
    fn default() -> Self {
        Self {
            open: HashMap::new(),
            kept: Arc::new(Mutex::new(HashSet::new())),
            held: Arc::new(AtomicU64::new(0)),
            idle: IDLE,
        }
    }
}

impl Sessions {
    /// When the soonest ready, untouched preview falls idle: how long the owner may sleep.
    pub(in crate::api::owner) fn next_idle(&self) -> Option<Instant> {
        self.open
            .values()
            .filter(|session| idle_eligible(session))
            .map(|session| session.last_used + self.idle)
            .min()
    }

    /// What a job's sweep keeps: the open previews' identities, read when it runs.
    pub(super) fn kept(&self) -> Arc<Mutex<HashSet<String>>> {
        self.kept.clone()
    }

    fn holding(&mut self, session: &mut Session, bytes: u64) {
        self.held.fetch_add(bytes, Ordering::SeqCst);
        session.held_bytes += bytes;
    }

    fn release(&self, session: &mut Session) {
        self.held.fetch_sub(session.held_bytes, Ordering::SeqCst);
        session.held_bytes = 0;
    }

    /// Open sessions, for a test.
    #[cfg(test)]
    pub(in crate::api::owner) fn count(&self) -> usize {
        self.open.len()
    }

    /// The open sessions' ids, for a test.
    #[cfg(test)]
    pub(in crate::api::owner) fn open_ids(&self) -> Vec<String> {
        self.open.keys().map(ToString::to_string).collect()
    }
}

/// A session that falls idle: ready, with no job of its own running and not being merged.
fn idle_eligible(session: &Session) -> bool {
    session.preparing.is_none() && session.update.is_none() && session.merging.is_none()
}

/// The previews' spool folder under the catalog's artifact root, refused with `incompatible` when
/// the root is another catalog's.
pub(super) fn previews_root(owner: &Owner) -> Result<PathBuf, Error> {
    let root = owner.service.artifact_root();
    if let RootState::Foreign(detail) = root_state(root, owner.service.catalog_id())? {
        return Err(Error::incompatible(detail));
    }
    Ok(root.join(PREVIEWS))
}

/// The previews' pictures folder in the catalog's preview cache.
pub(super) fn pictures_root(owner: &Owner) -> PathBuf {
    owner.service.index_dir().join(PREVIEWS_DIR).join(PICTURES)
}

/// Remove, as a job starts, the preview folders and pictures no open session keeps.
pub(super) fn sweep(previews: &Path, pictures: &Path, kept: &Mutex<HashSet<String>>) {
    let kept = kept.lock().map(|kept| kept.clone()).unwrap_or_default();
    preview::sweep(previews, pictures, &|name| kept.contains(name));
}

/// "merge preview merge-preview-… is not open".
fn not_open(preview_id: &MergePreviewId) -> Error {
    Error::conflict(format!(
        "merge preview {preview_id} is not open: it was closed, merged, left idle or opened by \
         another client; open another with merge.preview"
    ))
    .with_data(json!({"preview_id": preview_id}))
}

/// A revision's answer.
fn answer(
    preview_id: &MergePreviewId,
    revision: u32,
    kind: MergeKind,
    options: MergeOptions,
    drawn: Drawn,
) -> MergePreview {
    let mut answer = MergePreview {
        preview_id: preview_id.clone(),
        revision,
        kind,
        options,
        path: None,
        width: 0,
        height: 0,
        diagnostics: None,
        overlays: None,
        output: None,
        refusal: None,
    };
    match drawn {
        Drawn::Picture(picture) => {
            answer.path = Some(picture.path);
            answer.width = picture.width;
            answer.height = picture.height;
            answer.diagnostics = Some(picture.diagnostics);
            answer.overlays = Some(picture.overlays);
            answer.output = Some(picture.output);
        }
        Drawn::Refused(refusal) => answer.refusal = Some(refusal),
    }
    answer
}

/// The refusal a revision answered, as the error a merge of it fails with.
fn refused(refusal: &Value) -> Error {
    let message = refusal["message"]
        .as_str()
        .unwrap_or("the merge preview refused these frames")
        .to_owned();
    let error = if refusal["reason"] == "raw-only" {
        Error::unsupported_input(message)
    } else {
        Error::validation(message)
    };
    let mut data = refusal.clone();
    if let Some(object) = data.as_object_mut() {
        object.remove("message");
    }
    error.with_data(data)
}

/// Cancel one of a session's jobs, as `job.cancel` would: a waiting one ends now, a running one at
/// its next checkpoint.
fn cancel(owner: &mut Owner, job_id: &JobId) {
    owner
        .jobs
        .cancel(job_id, "the merge preview no longer needs it");
    owner.catalog_cancelled(job_id, JobKind::MergePreview);
    let lane = &owner.catalog.merge;
    if !lane.lane.holds(job_id) && !lane.updates.holds(job_id) {
        owner.release_waiters(|waiter| waiter.job.as_ref() == Some(job_id));
    }
}

/// Close a session, whatever it is doing but merging: its jobs cancelled, its spool, proxies and
/// pictures discarded off the owner.
fn close(owner: &mut Owner, preview_id: &MergePreviewId) -> bool {
    let sessions = &mut owner.catalog.merge.previews;
    let Some(mut session) = sessions.open.remove(preview_id) else {
        return false;
    };
    sessions.release(&mut session);
    if let Ok(mut kept) = sessions.kept.lock() {
        kept.remove(preview_id.as_str());
    }
    for job_id in [session.preparing.take(), session.update.take()]
        .into_iter()
        .flatten()
    {
        cancel(owner, &job_id);
    }
    discard(session);
    true
}

/// Drop a session's bracket and remove its folder and pictures on a thread of their own: removing
/// a spool is file work the owner does not wait for. A job still holding the bracket removes the
/// spool as it lets go of it.
fn discard(session: Session) {
    let Session {
        slot,
        folder,
        pictures,
        ..
    } = session;
    let _ = std::thread::Builder::new()
        .name("luxforge-merge-preview-discard".into())
        .spawn(move || {
            if let Ok(mut retained) = slot.lock() {
                retained.take();
            }
            drop(slot);
            let _ = fs::remove_dir_all(&folder);
            pictures.remove(None);
        });
}

/// Close every open preview of `client`, which has gone. A preview being merged is the merge's.
pub(in crate::api::owner) fn disconnect(owner: &mut Owner, client: ClientId) {
    let theirs: Vec<MergePreviewId> = owner
        .catalog
        .merge
        .previews
        .open
        .iter()
        .filter(|(_, session)| session.client == client && session.merging.is_none())
        .map(|(id, _)| id.clone())
        .collect();
    for preview_id in theirs {
        close(owner, &preview_id);
    }
}

/// Close every ready preview untouched for the idle limit.
pub(in crate::api::owner) fn expire(owner: &mut Owner) {
    let sessions = &owner.catalog.merge.previews;
    if sessions.open.is_empty() {
        return;
    }
    let now = Instant::now();
    let idle = sessions.idle;
    let due: Vec<MergePreviewId> = sessions
        .open
        .iter()
        .filter(|(_, session)| idle_eligible(session) && session.last_used + idle <= now)
        .map(|(id, _)| id.clone())
        .collect();
    for preview_id in due {
        close(owner, &preview_id);
    }
}

/// A merge or preview job ended, however it ended: a session it prepared that failed or was
/// cancelled is closed; one it updated is free for the next; one it merged is gone.
pub(super) fn ended(owner: &mut Owner, job_id: &JobId) {
    let failed = owner
        .jobs
        .read(job_id)
        .is_none_or(|record| record.error.is_some());
    let mut closing = None;
    let mut merged = None;
    for (preview_id, session) in &mut owner.catalog.merge.previews.open {
        if session.preparing.as_ref() == Some(job_id) {
            session.preparing = None;
            if failed {
                closing = Some(preview_id.clone());
            }
        } else if session.update.as_ref() == Some(job_id) {
            session.update = None;
            session.last_used = Instant::now();
        } else if session.merging.as_ref() == Some(job_id) {
            merged = Some(preview_id.clone());
        }
    }
    if let Some(preview_id) = closing {
        close(owner, &preview_id);
    }
    if let Some(preview_id) = merged {
        let sessions = &mut owner.catalog.merge.previews;
        if let Some(mut session) = sessions.open.remove(&preview_id) {
            sessions.release(&mut session);
            if let Ok(mut kept) = sessions.kept.lock() {
                kept.remove(preview_id.as_str());
            }
            discard(session);
        }
    }
}

// The methods.

/// `merge.preview`: open the calling client's preview of `targets`, closing its previous one, and
/// start its `merge-preview` job on the merge lane, answering `{job_id, status, deduplicated}`;
/// the job's result is the first revision's [`MergePreview`].
pub(in crate::api) fn merge_preview(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergePreviewParams,
) -> Result<Value, Error> {
    let kind = params.kind;
    let options = options(kind, params.hdr, params.panorama)?;
    let provider = owner.catalog.merge.available_provider(kind)?;
    let named = inputs(owner, call.client, kind, &params.targets)?;
    let parameters = requested(&named, &options)?;
    if let Some(plan_id) = &params.plan_id
        && *plan_id != super::plan_identity(&named, &parameters)?
    {
        return Err(Error::conflict(
            "the inputs or options are no longer what merge.plan planned; plan them again",
        )
        .with_data(json!({"plan_id": plan_id})));
    }
    let frames = planned_frames(&named);
    header_check(&named, &frames, &parameters)?;
    let signatures = signed(&named)?;
    // One open preview a client.
    let previous: Vec<MergePreviewId> = owner
        .catalog
        .merge
        .previews
        .open
        .iter()
        .filter(|(_, session)| session.client == call.client && session.merging.is_none())
        .map(|(id, _)| id.clone())
        .collect();
    for preview_id in previous {
        close(owner, &preview_id);
    }
    let lane = &owner.catalog.merge;
    if lane.lane.is_full() {
        return Err(Error::resource_limit(format!(
            "a merge is running and {} waits; wait for one to end",
            super::MERGE_QUEUE
        )));
    }
    let root = previews_root(owner)?;
    let merges = super::merges_root(&owner.service)?;
    let pictures_folder = pictures_root(owner);
    fs::create_dir_all(&root).map_err(|error| {
        crate::atomic_file::file_error("cannot create the merge previews' folder", error.kind())
    })?;
    let limits = lane.limits(kind);
    let held = lane.previews.held.clone();
    let n = named.len();
    if let Some(frame) = frames.first().filter(|frame| frame.width > 0) {
        let planned = match kind {
            MergeKind::Hdr => job::temp_bytes(n, frame.width, frame.height),
            MergeKind::Panorama => spool_bytes(n, frame.width, frame.height),
        };
        limits.preflight(&root, planned, held.load(Ordering::SeqCst))?;
    }
    let preview_id = MergePreviewId::new();
    let folder = root.join(preview_id.as_str());
    let pictures = Pictures {
        folder: pictures_folder.clone(),
        id: preview_id.to_string(),
    };
    let job_inputs: Vec<JobInput> = named
        .iter()
        .zip(signatures)
        .zip(frames.iter().cloned())
        .map(|((input, signature), planned)| JobInput {
            path: input.path.clone(),
            signature,
            asset_id: input.asset_id.clone(),
            fingerprint: None,
            planned,
        })
        .collect();
    let spec = super::Spec::new(
        lane,
        job_inputs.clone(),
        parameters.clone(),
        provider,
        folder.clone(),
        owner.service.derived_source_writer(),
        limits,
        held,
        owner.service.auto_lens_profile(),
    )?;
    let slot: Slot = Arc::new(Mutex::new(None));
    let kept = lane.previews.kept();
    if let Ok(mut kept) = kept.lock() {
        kept.insert(preview_id.to_string());
    }
    let reference = super::planned_reference(&frames, &parameters).unwrap_or((n - 1) / 2);
    let detail = match kind {
        MergeKind::Hdr => format!(
            "{} \u{b7} {n} exposures",
            job::result_name(&named[reference].path)
        ),
        MergeKind::Panorama => format!(
            "{} \u{b7} {n} photos",
            job::named_result(&named[reference].path, kind)
        ),
    };
    let job_id = JobId::new();
    let task = {
        let slot = slot.clone();
        let preview_id = preview_id.clone();
        let options = options.resolved();
        let pictures = pictures.clone();
        Task {
            job_id: job_id.clone(),
            job: &MERGE_PREVIEW,
            asset_id: None,
            detail: Some(detail),
            origin: call.origin.clone(),
            work: Box::new(move |context: &JobContext<'_>| {
                job::sweep_orphans(&merges, Path::new(""));
                sweep(&root, &pictures_folder, &kept);
                match first_revision(spec, &pictures, context.control) {
                    Ok((retained, bytes, drawn)) => {
                        if let (Some(retained), Ok(mut slot)) = (retained, slot.lock()) {
                            *slot = Some(retained);
                        }
                        // The frames stay with the slot alone: the owner's half holds none.
                        drop(slot);
                        Box::new(move |owner: &mut Owner| {
                            prepared(owner, &preview_id, kind, options, bytes, drawn)
                        })
                    }
                    Err(error) => Box::new(move |_| Err(error)),
                }
            }),
        }
    };
    let started = owner.catalog.merge.lane.queue(&mut owner.jobs, task);
    let started = match started {
        Ok(started) => started,
        Err(error) => {
            if let Ok(mut kept) = owner.catalog.merge.previews.kept.lock() {
                kept.remove(preview_id.as_str());
            }
            return Err(error);
        }
    };
    owner.catalog.merge.previews.open.insert(
        preview_id,
        Session {
            client: call.client,
            kind,
            named,
            inputs: job_inputs,
            revision: 1,
            parameters,
            answered: None,
            slot,
            unread: false,
            preparing: Some(job_id),
            update: None,
            merging: None,
            held_bytes: 0,
            last_used: Instant::now(),
            folder,
            pictures,
        },
    );
    value(started)
}

/// A preview's first job of its kind: the frames it read and kept (none when reading refused
/// them), the bytes they spool and the first revision.
fn first_revision(
    spec: super::Spec,
    pictures: &Pictures,
    control: &crate::jobs::JobControl,
) -> Result<(Option<Retained>, Option<u64>, Drawn), Error> {
    Ok(match spec {
        super::Spec::Hdr(spec) => {
            let previewed = spec.preview(pictures, 1, control)?;
            let bytes = previewed
                .prepared
                .as_ref()
                .map(|prepared| prepared.temp_bytes);
            let retained = previewed.prepared.map(|prepared| Retained::Hdr {
                prepared,
                analysis: previewed.analysis,
            });
            (retained, bytes, previewed.drawn)
        }
        super::Spec::Panorama(spec) => {
            let previewed = spec.preview(pictures, 1, control)?;
            let bytes = previewed
                .prepared
                .as_ref()
                .map(|prepared| prepared.temp_bytes);
            let retained = previewed.prepared.map(|prepared| Retained::Panorama {
                prepared,
                analysis: previewed.analysis,
            });
            (retained, bytes, previewed.drawn)
        }
    })
}

/// The owner's half of a preview's first job: the session ready (its spool counted against the
/// ceiling) or refused, and the first revision answered.
fn prepared(
    owner: &mut Owner,
    preview_id: &MergePreviewId,
    kind: MergeKind,
    options: MergeOptions,
    bytes: Option<u64>,
    drawn: Drawn,
) -> Result<Value, Error> {
    let sessions = &mut owner.catalog.merge.previews;
    let Some(mut session) = sessions.open.remove(preview_id) else {
        return Err(not_open(preview_id));
    };
    session.preparing = None;
    session.last_used = Instant::now();
    match bytes {
        Some(bytes) if session.merging.is_none() => sessions.holding(&mut session, bytes),
        Some(_) => {}
        None => session.unread = true,
    }
    let refusal = match &drawn {
        Drawn::Refused(refusal) => Some(refusal.clone()),
        Drawn::Picture(_) => None,
    };
    if session.answered.is_none() {
        session.answered = Some(Answered {
            revision: 1,
            refusal,
        });
    }
    sessions.open.insert(preview_id.clone(), session);
    value(answer(preview_id, 1, kind, options, drawn))
}

/// `merge.preview-update`: the calling client's open preview drawn again with the full new
/// options, from its retained proxies, as a short `merge-preview` job answering the next
/// revision; a stale update of the same preview is cancelled.
pub(in crate::api) fn merge_preview_update(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergePreviewUpdate,
) -> Result<Value, Error> {
    let preview_id = params.preview_id;
    let sessions = &owner.catalog.merge.previews;
    let session = sessions
        .open
        .get(&preview_id)
        .filter(|session| session.client == call.client)
        .ok_or_else(|| not_open(&preview_id))?;
    if session.merging.is_some() {
        return Err(
            Error::conflict(format!("merge preview {preview_id} is being merged"))
                .with_data(json!({"preview_id": preview_id})),
        );
    }
    let options = options(session.kind, params.hdr, params.panorama)?;
    let parameters = requested(&session.named, &options)?;
    // The header-level refusals merge.create would answer at once, from the planned frames.
    let frames: Vec<_> = session
        .inputs
        .iter()
        .map(|input| input.planned.clone())
        .collect();
    header_check(&session.named, &frames, &parameters)?;
    let stale = session.update.clone();
    if let Some(stale) = stale {
        cancel(owner, &stale);
    }
    let lane = &mut owner.catalog.merge;
    let kind = lane
        .previews
        .open
        .get(&preview_id)
        .map_or(MergeKind::Hdr, |session| session.kind);
    let limits = lane.limits(kind);
    let hooks = lane.hooks.clone();
    let stitcher = match kind {
        MergeKind::Panorama => Some(lane.stitcher()?),
        MergeKind::Hdr => None,
    };
    let session = lane
        .previews
        .open
        .get_mut(&preview_id)
        .ok_or_else(|| not_open(&preview_id))?;
    session.revision += 1;
    session.parameters = parameters.clone();
    session.last_used = Instant::now();
    let revision = session.revision;
    let options = options.resolved();
    if session.unread {
        // Reading refused the bracket: there are no frames to draw, and every revision answers
        // that refusal.
        let refusal = session
            .answered
            .as_ref()
            .and_then(|answered| answered.refusal.clone())
            .unwrap_or(Value::Null);
        session.answered = Some(Answered {
            revision,
            refusal: Some(refusal.clone()),
        });
        let drawn = answer(
            &preview_id,
            revision,
            kind,
            options,
            Drawn::Refused(refusal),
        );
        return value(LibraryLane::answered(
            &mut owner.jobs,
            &MERGE_PREVIEW,
            None,
            call.origin.clone(),
            serde_json::to_value(drawn)
                .map_err(|error| Error::internal(format!("cannot encode a preview: {error}")))?,
        ));
    }
    let slot = session.slot.clone();
    let preparing = session.preparing.clone();
    let update = Update {
        prepared_from: slot,
        parameters,
        stitcher,
        limits,
        hooks,
        pictures: session.pictures.clone(),
        revision,
    };
    let job_id = JobId::new();
    let task = {
        let preview_id = preview_id.clone();
        Task {
            job_id: job_id.clone(),
            job: &MERGE_PREVIEW,
            asset_id: None,
            detail: None,
            origin: call.origin.clone(),
            work: Box::new(move |context: &JobContext<'_>| {
                let job_id = context.job_id().clone();
                match update.run(context.control) {
                    Ok((analysis, drawn)) => Box::new(move |owner: &mut Owner| {
                        updated(
                            owner,
                            &preview_id,
                            &job_id,
                            revision,
                            options,
                            analysis,
                            drawn,
                        )
                    }),
                    Err(error) => Box::new(move |_| Err(error)),
                }
            }),
        }
    };
    let started = match preparing {
        // Still preparing: the update waits behind it on the merge lane.
        Some(preparing) => Ok(lane.lane.queue_after(&mut owner.jobs, task, &preparing)),
        None => lane.updates.queue(&mut owner.jobs, task),
    }?;
    if let Some(session) = lane.previews.open.get_mut(&preview_id) {
        session.update = Some(job_id);
    }
    value(started)
}

/// What an update job is given: the session's slot, read when it runs, the new options, a
/// panorama's stitch and where its picture goes.
struct Update {
    prepared_from: Slot,
    parameters: MergeParameters,
    stitcher: Option<Arc<dyn crate::merge::panorama::stitching::Stitcher>>,
    limits: job::Limits,
    hooks: job::Hooks,
    pictures: Pictures,
    revision: u32,
}

impl Update {
    /// Draw the revision, answering the analysis the session keeps for it.
    fn run(self, control: &crate::jobs::JobControl) -> Result<(Analysed, Drawn), Error> {
        let retained = self
            .prepared_from
            .lock()
            .ok()
            .and_then(|retained| retained.clone())
            .ok_or_else(|| {
                Error::conflict("the merge preview ended without frames to draw; open another")
            })?;
        match (retained, self.parameters, self.stitcher) {
            (Retained::Hdr { prepared, analysis }, MergeParameters::Hdr(parameters), _) => {
                let (analysis, drawn) = preview::Update {
                    prepared,
                    previous: analysis,
                    parameters,
                    limits: self.limits,
                    hooks: self.hooks,
                    pictures: self.pictures,
                    revision: self.revision,
                }
                .run(control)?;
                Ok((Analysed::Hdr(analysis), drawn))
            }
            (
                Retained::Panorama { prepared, analysis },
                MergeParameters::Panorama(parameters),
                Some(stitcher),
            ) => {
                let (analysis, drawn) = PanoramaUpdate {
                    prepared,
                    previous: analysis,
                    parameters,
                    stitcher,
                    limits: self.limits,
                    hooks: self.hooks,
                    pictures: self.pictures,
                    revision: self.revision,
                }
                .run(control)?;
                Ok((Analysed::Panorama(analysis), drawn))
            }
            _ => Err(Error::internal(
                "a merge preview's update of another kind than its frames",
            )),
        }
    }
}

/// What an update leaves for its session to keep: the revision's alignment or registration, of
/// the preview's kind; none when the revision was refused.
enum Analysed {
    Hdr(Option<Arc<Analysis>>),
    Panorama(Option<Arc<PanoramaAnalysis>>),
}

/// The owner's half of an update: the session's latest alignment and answer, unless a newer
/// revision was asked for meanwhile.
fn updated(
    owner: &mut Owner,
    preview_id: &MergePreviewId,
    job_id: &JobId,
    revision: u32,
    options: MergeOptions,
    analysis: Analysed,
    drawn: Drawn,
) -> Result<Value, Error> {
    let session = owner
        .catalog
        .merge
        .previews
        .open
        .get_mut(preview_id)
        .ok_or_else(|| not_open(preview_id))?;
    if session.revision != revision || session.merging.is_some() {
        return Err(Error::cancelled(format!(
            "merge preview {preview_id} has a newer revision than {revision}"
        )));
    }
    if session.update.as_ref() == Some(job_id) {
        session.update = None;
    }
    session.last_used = Instant::now();
    if let Ok(mut retained) = session.slot.lock()
        && let Some(retained) = retained.as_mut()
    {
        match (retained, analysis) {
            (Retained::Hdr { analysis: kept, .. }, Analysed::Hdr(analysis)) => *kept = analysis,
            (Retained::Panorama { analysis: kept, .. }, Analysed::Panorama(analysis)) => {
                *kept = analysis;
            }
            _ => {}
        }
    }
    session.answered = Some(Answered {
        revision,
        refusal: match &drawn {
            Drawn::Refused(refusal) => Some(refusal.clone()),
            Drawn::Picture(_) => None,
        },
    });
    let kind = session.kind;
    value(answer(preview_id, revision, kind, options, drawn))
}

/// `merge.preview-close`: discard the calling client's preview, its spool and pictures, answering
/// `{preview_id, closed}`; a preview not open (or being merged) is left as it is.
pub(in crate::api) fn merge_preview_close(
    owner: &mut Owner,
    call: &Call<'_>,
    params: MergePreviewClose,
) -> Result<Value, Error> {
    let preview_id = params.preview_id;
    let theirs = owner
        .catalog
        .merge
        .previews
        .open
        .get(&preview_id)
        .is_some_and(|session| session.client == call.client && session.merging.is_none());
    let closed = theirs && close(owner, &preview_id);
    Ok(json!({"preview_id": preview_id, "closed": closed}))
}

// Merging a preview.

/// A merge continuing a preview: the slot it takes the bracket from, and the preview job it waits
/// behind while that is still preparing.
pub(super) struct Continuation {
    pub preview_id: MergePreviewId,
    pub slot: Slot,
    pub after: Option<JobId>,
}

/// The preview a `merge.create` names, checked: the calling client's open preview of the same
/// kind, inputs and signatures, whose latest revision's options are the request's, and which that
/// revision did not refuse. `conflict` otherwise; the refusal's own error when it was refused.
pub(super) fn continuation(
    owner: &Owner,
    client: ClientId,
    preview_id: &MergePreviewId,
    kind: MergeKind,
    named: &[Named],
    parameters: &MergeParameters,
) -> Result<Continuation, Error> {
    let session = owner
        .catalog
        .merge
        .previews
        .open
        .get(preview_id)
        .filter(|session| session.client == client)
        .ok_or_else(|| not_open(preview_id))?;
    if session.merging.is_some() {
        return Err(Error::conflict(format!(
            "merge preview {preview_id} is being merged already"
        ))
        .with_data(json!({"preview_id": preview_id})));
    }
    let same_inputs = session.kind == kind
        && session.named.len() == named.len()
        && session
            .named
            .iter()
            .zip(named)
            .all(|(previewed, named)| previewed.path == named.path);
    if !same_inputs {
        return Err(Error::conflict(format!(
            "merge preview {preview_id} is of other frames than this merge names"
        ))
        .with_data(json!({"preview_id": preview_id})));
    }
    if session.parameters != *parameters {
        return Err(Error::conflict(format!(
            "the options are not merge preview {preview_id}'s latest (revision {}); merge with \
             its options or update it first",
            session.revision
        ))
        .with_data(json!({"preview_id": preview_id, "revision": session.revision})));
    }
    for (input, named) in session.inputs.iter().zip(named) {
        if !named
            .signature
            .is_some_and(|signature| signature.unchanged(&input.signature))
        {
            return Err(Error::conflict(format!(
                "{} changed since merge preview {preview_id} read it; open another",
                super::file_name(&input.path)
            ))
            .with_data(json!({"preview_id": preview_id, "path": input.path})));
        }
    }
    if let Some(answered) = &session.answered
        && (session.unread || answered.revision == session.revision)
        && let Some(refusal) = &answered.refusal
    {
        return Err(refused(refusal));
    }
    Ok(Continuation {
        preview_id: preview_id.clone(),
        slot: session.slot.clone(),
        after: session.preparing.clone(),
    })
}

/// The inputs a merge of a preview records: the preview's, with the signatures it read.
pub(super) fn preview_inputs(owner: &Owner, preview_id: &MergePreviewId) -> Option<Vec<JobInput>> {
    owner
        .catalog
        .merge
        .previews
        .open
        .get(preview_id)
        .map(|session| session.inputs.clone())
}

/// The merge `job_id` took the preview over: the session is merging, no longer counted as held
/// (the merge's own preflight counts it), and any update it had is cancelled.
pub(super) fn merging(owner: &mut Owner, preview_id: &MergePreviewId, job_id: &JobId) {
    let sessions = &mut owner.catalog.merge.previews;
    let Some(mut session) = sessions.open.remove(preview_id) else {
        return;
    };
    sessions.release(&mut session);
    session.merging = Some(job_id.clone());
    let stale = session.update.take();
    sessions.open.insert(preview_id.clone(), session);
    if let Some(stale) = stale {
        cancel(owner, &stale);
    }
}

/// The bracket a continuing merge takes when it runs: the slot's, emptied, so the merge holds it
/// alone; none when the preview ended without frames.
pub(super) fn take(slot: &Slot) -> Result<Retained, Error> {
    slot.lock()
        .ok()
        .and_then(|mut retained| retained.take())
        .ok_or_else(|| {
            Error::conflict(
                "the merge preview this merge continues ended without frames to merge; nothing \
                 was merged",
            )
        })
}
