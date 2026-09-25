//! The owner tasks. Every desktop request goes through `call`, which is the same method table the
//! JSON API dispatches; there is no desktop-only mutation path. Each task takes the narrowest
//! completion path the performance rules allow.
use crate::{
    app::{
        draft::GestureId,
        message::{
            DraftMessage, EvidenceMessage, HistoryMessage, MaskMessage, Message,
            PerformanceMessage, PointerMessage, PresetMessage, PreviewMessage, SyncMessage,
            ViewMessage,
        },
    },
    state::histogram::Readout,
};
use iced::Task;
use lightwell_core::{
    ActionResult, ApiRequest, AssetId, ClientId, ClientSession, ContentPoint, Draft, DraftId,
    EditorState, EntryId, ErrorKind, EventsResult, HistoryPage, HistoryRow, HistorySelection,
    Lineage, MAX_PRESET_BYTES, ModuleDescriptor, Mutation, MutationOutcome, MutationRequest,
    OwnerHandle, PresetSummary, PreviewJob, PreviewRequest, ProxyBounds, RecipeDescription,
    StageTransform, Version,
    mask::commands::{MaskListing, MaskTarget},
};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

pub(crate) static REQUEST_NUMBER: AtomicU64 = AtomicU64::new(1);
pub(crate) const HISTORY_PAGE_SIZE: usize = 50;
const LINEAGE_LIMIT: usize = 100;
pub(crate) const ACTOR: &str = "desktop";

/// Authoritative state read back from the owner after a change, as much of it as the change's
/// [`Scope`] could have touched. A part that is `None` was not read, and the desktop keeps what it
/// holds or brings it forward itself.
#[derive(Clone, Debug)]
pub(crate) struct Refresh {
    pub(crate) state: EditorState,
    /// The newest page of history rows, read when an asset opens or changed elsewhere. `None`
    /// merges the current entry's row into the loaded page.
    pub(crate) history: Option<HistoryPage>,
    /// Read when an asset opens or changed elsewhere; a command of this desktop's names no version,
    /// and a version's own methods read the list in their own task.
    pub(crate) versions: Option<Vec<Version>>,
    /// Read unless the change was this desktop's own commit, whose entry continues the chain from
    /// the entry that was current and joins the loaded lineage on the desktop.
    pub(crate) lineage: Option<Lineage>,
    /// The displayed entry's layers as the recipe panel reads them.
    pub(crate) recipe: RecipeDescription,
    /// The current entry's layers while another entry is displayed, for what follows the current
    /// entry rather than the displayed one: a section's edited dot. `None` when `recipe` is the
    /// current entry's own.
    pub(crate) current_recipe: Option<RecipeDescription>,
    /// The same entry's masks. It is read beside the recipe and never on its own, so the panel can
    /// never show a mask list and a layer list that describe two different entries.
    pub(crate) masks: MaskListing,
    /// The Original entry, read once when an asset opens so Compare needs no search.
    pub(crate) original: Option<EntryId>,
    pub(crate) job: PreviewJob,
    pub(crate) session: ClientSession,
    /// This desktop's own request whose change the refresh read back: the event that request left
    /// in the log needs no refresh of its own when a poll reads it ([`sync_now`]). `None` when the
    /// refresh read back no change of this desktop's.
    pub(crate) request: Option<String>,
}

/// What a refresh reads back, by what the change before it could have touched. Every scope reads
/// `asset.state`, the session, the displayed entry's recipe rows and masks, and one preview job;
/// the rest is read only where the change could have moved it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Scope {
    /// An asset opened: the newest history page, its versions and lineage, and the Original's id,
    /// which is read here once for the asset and never again.
    Open,
    /// Another client changed the asset, or a gap in the event log could have hidden any change:
    /// everything an open reads but the Original, which never changes.
    Elsewhere,
    /// This desktop's own commit, answered at this revision. Its entry merges into the loaded page
    /// and joins the loaded lineage on the desktop, so neither is read.
    Commit(u64),
    /// This desktop's own undo, redo or restore, answered at this revision. The current entry moved
    /// along or off the chain, so the lineage is read; the page takes the merge as a commit's does.
    Navigate(u64),
}

impl Scope {
    /// The scope of a mutation from what `method` answered. Undo, redo and restore navigate; every
    /// other command that answers a revision commits. An answer without one says nothing about what
    /// it touched, so it is read as a change made elsewhere.
    pub(crate) fn after(method: &str, answer: &Value) -> Self {
        let Some(revision) = answer["revision"].as_u64() else {
            return Self::Elsewhere;
        };
        match method {
            "history.undo" | "history.redo" | "history.restore" => Self::Navigate(revision),
            _ => Self::Commit(revision),
        }
    }

    /// The scope once `asset.state` has been read. A commit or a navigation is merged on the desktop
    /// only when the state read is the one the command left: a later revision means another change
    /// landed in between, which a merge of the current entry would miss, so it is read as one.
    fn at(self, revision: u64) -> Self {
        match self {
            Self::Commit(answered) | Self::Navigate(answered) if answered != revision => {
                Self::Elsewhere
            }
            scope => scope,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct PreviewPayload {
    pub(crate) job: PreviewJob,
    pub(crate) session: ClientSession,
}

#[derive(Clone, Debug)]
pub(crate) struct Upload {
    pub(crate) generation: u64,
    /// The draft revision this frame was rendered from, when a draft produced it.
    pub(crate) draft_revision: Option<u64>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) entry_id: EntryId,
    pub(crate) snapshot_id: String,
    pub(crate) source_fingerprint: String,
    /// These pixels are the display proxy of the frame, not its exact render. `width`/`height`
    /// above are the texture's own size, which at a proxy is the proxy's; the exact stage the
    /// picks, the percent box and the overlay grid map through stays on `Editor::dimensions`.
    pub(crate) proxy: bool,
    /// The proxy source dimensions this frame was rendered against, when it is one.
    pub(crate) proxy_dimensions: Option<(u32, u32)>,
    /// The proxy source was built for this frame rather than taken from the queue's cache.
    pub(crate) proxy_built: bool,
    /// Whether these proxy pixels approximate the exact render at display size, and why: a
    /// spatial-stage layer whose neighbourhoods scale with the stage, a mask drawing a feature
    /// narrower than two proxy pixels, or both.
    pub(crate) proxy_approximation: lightwell_core::ProxyApproximation,
    /// These pixels approximate a drafted RAW white balance on planes developed at another one,
    /// at either phase ([`lightwell_core::PreviewResult::approximate_white_balance`]).
    pub(crate) approximate_white_balance: bool,
    /// Why these pixels are being uploaded when no render asked for it: `Some("zoom")` is the
    /// retained raster a zoom change needed. `None` is the ordinary path, a rendered frame.
    pub(crate) reason: Option<&'static str>,
    /// How long these pixels took to render: the preview worker's own time for the phase that
    /// produced them, carried with a retained frame when a zoom hands it back. `None` only when
    /// nothing recorded one for a retained raster.
    pub(crate) render_ms: Option<f64>,
}

/// What one live-refresh poll found. One poll answers every kind of change: the asset's state is
/// read back when an event that is neither a preset nor a capability one arrived, the preset
/// library when a `preset.*` event did, and `capabilities` says a capability method (`module.*` or
/// `task.*`) was used, whose changes the asset state does not show; a gap in the log, which could
/// have hidden any of them, asks for all three.
///
/// The poll is the only reader of the event log, so its `sequence` is the only one the desktop's
/// event cursor ever takes: the log's newest sequence as `events.since` answered it, which every
/// event the poll acted on is at or below. The sequence any other response carries counts other
/// clients' events this desktop has not read, and is never a cursor.
#[derive(Clone, Debug)]
pub(crate) struct SyncResult {
    pub(crate) sequence: u64,
    pub(crate) refresh: Option<Box<Refresh>>,
    /// The listing and the event sequence it was read at.
    pub(crate) presets: Option<(Vec<PresetSummary>, u64)>,
    pub(crate) capabilities: bool,
    /// This desktop's own requests whose events the poll read and skipped, because the answer to
    /// each had already read its change back.
    pub(crate) own: Vec<String>,
}

#[cfg(test)]
impl SyncResult {
    /// A poll that saw an asset event and read the state back. It read events up to sequence 0,
    /// which moves no cursor; a test that needs one sets `sequence`.
    pub(crate) fn changed(refresh: Refresh) -> Self {
        Self {
            sequence: 0,
            refresh: Some(Box::new(refresh)),
            presets: None,
            capabilities: false,
            own: Vec::new(),
        }
    }
}

/// A capability method's event: it changes a module's settings, grants, resources, activation or
/// jobs, never an asset's history.
pub(crate) fn capability_event(method: &str) -> bool {
    method.starts_with("module.") || method.starts_with("task.")
}
/// One library call of this desktop's and the listing read right after it, so the section shows
/// the library the call left behind rather than the one before it.
#[derive(Clone, Debug)]
pub(crate) struct PresetChange {
    /// What the call itself answered.
    pub(crate) result: Value,
    pub(crate) presets: Vec<PresetSummary>,
    /// The event sequence the listing was read at, which orders it against other listings.
    pub(crate) sequence: u64,
    /// The call's own request, whose event the listing already reflects.
    pub(crate) request: String,
}

/// A host method an evidence script called directly, and what it answered.
#[derive(Clone, Debug)]
pub(crate) struct HostAnswer {
    pub(crate) method: String,
    pub(crate) result: Value,
    /// The library listed after the call, when the method is one of the library's own.
    pub(crate) presets: Option<Vec<PresetSummary>>,
    pub(crate) sequence: u64,
}

/// Offer a job the display bounds the caller computed, when there are any.
///
/// The bounds are decided in `update`, on the thread that owns the window, and travel with the
/// request: a task runs off-thread and must not read the editor. `None` asks for the exact path
/// alone, which is what a zoom at or above 100% and a truncated crop-draft job take.
fn proxied(request: PreviewRequest, proxy: Option<ProxyBounds>) -> PreviewRequest {
    match proxy {
        Some(bounds) => request.proxy(bounds),
        None => request,
    }
}

/// A fresh request identity, so a retry of the same press is recognised and a new press is not.
fn request_id() -> String {
    format!(
        "desktop-{}-{}",
        std::process::id(),
        REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
    )
}

/// The envelope of a change to something with a revision: an asset or a module's settings.
pub(crate) fn mutation(revision: u64) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: request_id(),
        actor: ACTOR.into(),
    }
}

/// The envelope of every other change: the preset library, versions, an import, and module
/// permissions, activation, resources and jobs.
pub(crate) fn request() -> MutationRequest {
    MutationRequest {
        request_id: request_id(),
        actor: ACTOR.into(),
    }
}

/// One request through the owner's method table: its answer and the event log's sequence when it
/// was answered. That sequence counts every client's events, including ones this desktop has not
/// read, so it orders answers and is never where the event sync reads from.
pub(crate) fn call(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<(Value, u64), String> {
    send(owner, client, api_request_id(), method, params)
}

/// One change of this desktop's own whose answer the caller reads back: its answer and the request
/// id its event carries, which the caller hands to the event sync once the read-back is on screen.
pub(crate) fn call_own(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<(Value, String), String> {
    let id = api_request_id();
    let (answer, _) = send(owner, client, id.clone(), method, params)?;
    Ok((answer, id))
}

/// A request id no other client's is likely to repeat, so an event is recognised as this desktop's
/// own by its id alone.
fn api_request_id() -> String {
    format!(
        "ui-{}-{}",
        std::process::id(),
        REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
    )
}

fn send(
    owner: &OwnerHandle,
    client: ClientId,
    id: String,
    method: &str,
    params: Value,
) -> Result<(Value, u64), String> {
    #[cfg(test)]
    owner_calls::record(method);
    let request = ApiRequest {
        id,
        method: method.into(),
        params,
        token: None,
    };
    let response = owner
        .call(client, request)
        .map_err(|error| error.to_string())?;
    if let Some(error) = response.error {
        Err(format!("{}: {}", error.code, error.message))
    } else {
        Ok((response.result.unwrap_or(Value::Null), response.sequence))
    }
}

/// One preview job from the owner: a request on the owner thread like `call`, which is not a JSON
/// method, so it is counted here.
fn plan_preview(
    owner: &OwnerHandle,
    request: PreviewRequest,
) -> Result<PreviewJob, lightwell_core::Error> {
    #[cfg(test)]
    owner_calls::record("preview_job");
    owner.preview_job(request)
}

/// The owner requests this thread made, in order, so a test can count what one completion path
/// costs. Thread-local, so tests running beside each other count only their own.
#[cfg(test)]
pub(crate) mod owner_calls {
    use std::cell::RefCell;

    thread_local! {
        static CALLS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(method: &str) {
        CALLS.with(|calls| calls.borrow_mut().push(method.to_owned()));
    }

    /// Every request since the last take, in the order it was made.
    pub(crate) fn take() -> Vec<String> {
        CALLS.with(|calls| std::mem::take(&mut *calls.borrow_mut()))
    }
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|error| error.to_string())
}

/// Poll only while this client's bounded source job is active. Every poll is a short owner
/// request; decoding and hashing remain on the source worker.
fn wait_source_job(
    owner: &OwnerHandle,
    client: ClientId,
    job_id: &str,
    open_guard: Option<(&AtomicU64, u64)>,
) -> Result<EditorState, String> {
    loop {
        if open_guard.is_some_and(|(guard, generation)| guard.load(Ordering::Acquire) != generation)
        {
            let _ = call(owner, client, "job.cancel", json!({"job_id":job_id}));
            return Err("superseded open".into());
        }
        let (status, _) = call(owner, client, "job.status", json!({"job_id":job_id}))?;
        match status["status"].as_str() {
            Some("ready") => return parse(status["asset"].clone()),
            Some("failed") => {
                return Err(format!(
                    "{}: {}",
                    status["error"]["code"].as_str().unwrap_or("internal"),
                    status["error"]["message"]
                        .as_str()
                        .unwrap_or("source preparation failed")
                ));
            }
            Some("queued" | "running") => std::thread::sleep(std::time::Duration::from_millis(50)),
            _ => return Err("unexpected source job status".into()),
        }
    }
}

/// One preview job, waiting for the source preparation it needs first. Whether the job is still
/// wanted is not asked here: the desktop decides that when the answer arrives, from the session
/// generation and the asset revision the answer carries beside the job ([`super::Editor`]'s
/// `superseded`), and the preview queue's own generation keeps an older frame from following a
/// newer one on screen.
fn ready_preview_job(owner: &OwnerHandle, request: PreviewRequest) -> Result<PreviewJob, String> {
    let client = request.client;
    loop {
        match plan_preview(owner, request.clone()) {
            Ok(job) => return Ok(job),
            Err(error) if error.kind == lightwell_core::ErrorKind::PreparationRequired => {
                let job = error
                    .preparation_job()
                    .ok_or_else(|| error.to_string())?
                    .to_string();
                let _ = wait_source_job(owner, client, &job, None)?;
            }
            Err(error)
                if error.kind == lightwell_core::ErrorKind::ResourceLimit
                    && (error.detail.starts_with("RAW mosaic queue is full")
                        || error.detail.starts_with("source preparation queue is full")) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Read authoritative state back after a change or an external event, as much of it as `scope`
/// says the change could have touched.
pub(crate) fn refresh(
    owner: &OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    scope: Scope,
    proxy: Option<ProxyBounds>,
) -> Result<Refresh, String> {
    let fetch = |method: &str, params: Value| -> Result<Value, String> {
        call(owner, client, method, params).map(|(value, _)| value)
    };
    let state: EditorState = parse(fetch("asset.state", json!({"asset_id":asset_id}))?)?;
    let scope = scope.at(state.revision);
    let whole = matches!(scope, Scope::Open | Scope::Elsewhere);
    let history = if whole {
        Some(parse::<HistoryPage>(fetch(
            "history.list",
            json!({"asset_id":asset_id,"before_sequence":null,"limit":HISTORY_PAGE_SIZE}),
        )?)?)
    } else {
        None
    };
    let versions = if whole {
        Some(parse::<Vec<Version>>(
            fetch("version.list", json!({"asset_id":asset_id}))?["versions"].take(),
        )?)
    } else {
        None
    };
    let lineage = if matches!(scope, Scope::Commit(_)) {
        None
    } else {
        Some(parse::<Lineage>(fetch(
            "history.lineage",
            json!({"asset_id":asset_id,"limit":LINEAGE_LIMIT}),
        )?)?)
    };
    let session: ClientSession = parse(fetch("session.state", json!({}))?)?;
    // The entry the screen will show, named rather than left to the owner, so the recipe rows, the
    // masks and the preview job all describe the entry this state names even if another client
    // commits while they are read. That commit's own event brings its state and frame.
    let displayed = match &session.preview.selection {
        HistorySelection::Current => state.current_entry.id.clone(),
        HistorySelection::Entry(entry_id) => entry_id.clone(),
    };
    // The recipe rows of the entry that will be displayed: O(layers) payload reads, no render.
    let recipe: RecipeDescription = parse(fetch(
        "recipe.describe",
        json!({"asset_id":asset_id,"entry_id":displayed}),
    )?)?;
    // A historical preview leaves the current entry's rows unread, and a section's dot follows the
    // current entry, so they are read too: one more O(layers) payload read, only while previewing.
    let current_recipe = match &session.preview.selection {
        HistorySelection::Entry(_) => Some(parse::<RecipeDescription>(fetch(
            "recipe.describe",
            json!({"asset_id":asset_id,"entry_id":state.current_entry.id}),
        )?)?),
        HistorySelection::Current => None,
    };
    // The masks of that same entry. `recipe.describe` names each layer's mask and `mask.list` names
    // each mask's layers, so reading both together is what lets the panel show the relation from
    // either side without a second round trip.
    let masks: MaskListing = parse(fetch(
        "mask.list",
        mask_list_params(&asset_id, &Some(displayed.clone())),
    )?)?;
    // The Original entry is sequence 0: the last row of a page that reaches it, or else the one row
    // before sequence 1. Read once, when the asset opens.
    let original = match (scope, &history) {
        (Scope::Open, Some(page)) => match page.entries.last().filter(|row| row.sequence == 0) {
            Some(row) => Some(row.id.clone()),
            None => parse::<HistoryPage>(fetch(
                "history.list",
                json!({"asset_id":asset_id,"before_sequence":1,"limit":1}),
            )?)?
            .entries
            .first()
            .map(|row| row.id.clone()),
        },
        _ => None,
    };
    // Every preview of the displayed target is reduced by the same worker that rendered it, so
    // the histogram needs no second render and an `analysis.request` for this identity is a
    // cache hit. A truncated crop-draft job is the one exception; the core refuses to analyse
    // it, because its identity describes the whole stack rather than the prefix it renders.
    let job = ready_preview_job(
        owner,
        proxied(
            PreviewRequest::new(client, asset_id)
                .entry(Some(displayed))
                .analyse(),
            proxy,
        ),
    )?;
    Ok(Refresh {
        state,
        history,
        versions,
        lineage,
        recipe,
        current_recipe,
        masks,
        original,
        job,
        session,
        request: None,
    })
}

/// Discovery runs once: the controls on screen are whatever the registered modules declare.
pub(crate) fn modules_task(owner: OwnerHandle, client: ClientId) -> Task<Message> {
    Task::perform(
        async move {
            let (mut listed, _) = call(&owner, client, "module.list", json!({}))?;
            parse::<Vec<ModuleDescriptor>>(listed["modules"].take())
        },
        |value| Message::Sync(SyncMessage::ModulesLoaded(value)),
    )
}

pub(crate) fn import_task(
    owner: OwnerHandle,
    client: ClientId,
    path: PathBuf,
    generation: u64,
    open_guard: Arc<AtomicU64>,
    proxy: Option<ProxyBounds>,
    queued: Option<Result<QueuedImport, String>>,
) -> Task<Message> {
    Task::perform(
        async move {
            import_now(
                &owner,
                client,
                &path,
                generation,
                &open_guard,
                proxy,
                queued,
            )
        },
        move |result| {
            Message::Sync(SyncMessage::ImportRefreshed(
                generation,
                result.map(Box::new),
            ))
        },
    )
}

fn import_now(
    owner: &OwnerHandle,
    client: ClientId,
    path: &Path,
    generation: u64,
    open_guard: &AtomicU64,
    proxy: Option<ProxyBounds>,
    queued: Option<Result<QueuedImport, String>>,
) -> Result<Refresh, String> {
    let QueuedImport { job_id, request } = match queued {
        Some(result) => result?,
        None => queue_import(owner, client, path)?,
    };
    let state = wait_source_job(owner, client, &job_id, Some((open_guard, generation)))?;
    if open_guard.load(Ordering::Acquire) != generation {
        let _ = call(owner, client, "job.cancel", json!({"job_id":job_id}));
        return Err("superseded open".into());
    }
    call(owner, client, "job.adopt", json!({"job_id":job_id}))?;
    let mut refreshed = refresh(owner, client, state.asset.id, Scope::Open, proxy)?;
    if open_guard.load(Ordering::Acquire) != generation {
        return Err("superseded open".into());
    }
    // An import is announced under the `catalog.import` request that asked for it.
    refreshed.request = Some(request);
    Ok(refreshed)
}

/// The initial request can begin before the platform event loop and still finish through the
/// ordinary open path. Its clock includes startup work.
#[derive(Debug)]
pub(crate) struct QueuedImport {
    job_id: String,
    request: String,
}

pub(crate) struct StartupImport {
    pub(crate) started: Instant,
    pub(crate) result: Result<QueuedImport, String>,
}

pub(crate) fn start_import(owner: &OwnerHandle, client: ClientId, path: &Path) -> StartupImport {
    let started = Instant::now();
    let result = queue_import(owner, client, path);
    StartupImport { started, result }
}

fn queue_import(
    owner: &OwnerHandle,
    client: ClientId,
    path: &Path,
) -> Result<QueuedImport, String> {
    let (result, request) = call_own(
        owner,
        client,
        "catalog.import",
        json!({"path":path,"mutation":request()}),
    )?;
    let job_id = result["job_id"]
        .as_str()
        .ok_or("catalog.import did not return a source job")?
        .to_owned();
    Ok(QueuedImport { job_id, request })
}

/// One command and the refresh its answer calls for, as the plain calls [`state_task`] runs: an
/// ordinary commit reads `asset.state`, the session, the displayed entry's recipe rows and masks and
/// one preview job; undo, redo and restore add the lineage ([`Scope`]).
pub(crate) fn command_now(
    owner: &OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    method: &str,
    params: Value,
    proxy: Option<ProxyBounds>,
) -> Result<Refresh, String> {
    let (answer, request) = call_own(owner, client, method, params)?;
    let scope = Scope::after(method, &answer);
    let mut refreshed = refresh(owner, client, asset_id, scope, proxy)?;
    refreshed.request = Some(request);
    Ok(refreshed)
}

pub(crate) fn state_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    method: String,
    params: Value,
    proxy: Option<ProxyBounds>,
) -> Task<Message> {
    Task::perform(
        async move { command_now(&owner, client, asset_id, &method, params, proxy) },
        |result| Message::Sync(SyncMessage::Refreshed(result.map(Box::new))),
    )
}

/// One selection method and the preview job of what it selects, answered as `answered` says: a
/// history selection's answer is the one that clears `busy`, a comparison's is not.
#[allow(clippy::too_many_arguments)]
pub(crate) fn preview_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    entry_id: Option<EntryId>,
    method: &'static str,
    params: Value,
    proxy: Option<ProxyBounds>,
    answered: fn(Result<Box<PreviewPayload>, String>) -> Message,
) -> Task<Message> {
    Task::perform(
        async move {
            let (mut result, _) = call(&owner, client, method, params)?;
            let session: ClientSession = parse(result["session"].take())?;
            let job = ready_preview_job(
                &owner,
                proxied(
                    PreviewRequest::new(client, asset_id)
                        .entry(entry_id)
                        .analyse(),
                    proxy,
                ),
            )?;
            Ok(PreviewPayload { job, session })
        },
        move |result| answered(result.map(Box::new)),
    )
}

/// The displayed entry's layers and masks, read after a history selection changed which entry is
/// shown. It reads payloads only: no decode, no render, no source access.
pub(crate) fn recipe_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    entry_id: Option<EntryId>,
) -> Task<Message> {
    Task::perform(
        async move {
            let (described, _) = call(
                &owner,
                client,
                "recipe.describe",
                json!({"asset_id":asset_id,"entry_id":entry_id}),
            )?;
            let (masks, _) = call(
                &owner,
                client,
                "mask.list",
                mask_list_params(&asset_id, &entry_id),
            )?;
            Ok(RecipeRead {
                recipe: parse::<RecipeDescription>(described)?,
                masks: parse::<MaskListing>(masks)?,
            })
        },
        |result| Message::Sync(SyncMessage::RecipeDescribed(result.map(Box::new))),
    )
}

/// The `mask.list` request for one entry. The entry is omitted rather than sent as null, because
/// the method's optional envelope field takes an identity or nothing at all — the session's own
/// selection is what answers when it is absent.
fn mask_list_params(asset_id: &AssetId, entry_id: &Option<EntryId>) -> Value {
    match entry_id {
        Some(entry_id) => json!({"asset_id":asset_id,"entry_id":entry_id}),
        None => json!({ "asset_id": asset_id }),
    }
}

/// One entry's layers and masks, always read together.
#[derive(Clone, Debug)]
pub(crate) struct RecipeRead {
    pub(crate) recipe: RecipeDescription,
    pub(crate) masks: MaskListing,
}

/// The crop draft's only preview job: the stack truncated to the layers before the crop layer, which
/// is exactly that layer's input stage. Starting a draft and reapplying it are the only two requests.
///
/// A draft the crop displaced — an armed brush's — is cancelled first, in this same task, so the
/// job is planned once the owner no longer holds it. The job names the entry it truncated, and the
/// desktop opens the draft on it only while that entry is still the current one it holds.
pub(crate) fn crop_preview_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    layer_count: usize,
    displaced: Option<DraftId>,
) -> Task<Message> {
    Task::perform(
        async move {
            if let Some(draft_id) = displaced {
                let _ = call(&owner, client, "draft.cancel", json!({"draft_id":draft_id}));
            }
            ready_preview_job(
                &owner,
                PreviewRequest::new(client, asset_id).layers(layer_count),
            )
        },
        |result| {
            Message::Crop(crate::app::message::CropMessage::PreviewReady(
                result.map(Box::new),
            ))
        },
    )
}

/// Open this client's one draft for a gesture. The gesture sends nothing else until this answers,
/// so the draft identity every later request needs is known before any of them; the answer names
/// the gesture that asked, because the draft's own identity is what it brings.
///
/// The target is the host's own envelope: for a module action it is the mask the panel's sections
/// are bound to, so the drafted preview shows the masked layer the release will commit rather than
/// the global one; for a `mask.*` command it is the mask and component the gesture edits, which no
/// declared parameter kind could carry. A global gesture sends neither and drafts as it always has.
///
/// A draft the gesture displaced — an armed brush's — is cancelled first, in this same task, so the
/// begin can never reach the owner while it still holds that draft.
pub(crate) fn draft_begin_task(
    owner: OwnerHandle,
    client: ClientId,
    gesture: GestureId,
    asset_id: AssetId,
    action: String,
    target: MaskTarget,
    displaced: Option<DraftId>,
) -> Task<Message> {
    Task::perform(
        async move {
            if let Some(draft_id) = displaced {
                let _ = call(&owner, client, "draft.cancel", json!({"draft_id":draft_id}));
            }
            let (draft, _) = call(
                &owner,
                client,
                "draft.begin",
                draft_begin_params(asset_id, &action, target),
            )?;
            parse::<Draft>(draft)
        },
        move |result| {
            Message::Draft(DraftMessage::Begun {
                gesture,
                result: result.map(Box::new),
            })
        },
    )
}

/// The `draft.begin` request one action and target produce. One spelling for every gesture, so no
/// two can disagree about where an identity goes.
pub(crate) fn draft_begin_params(asset_id: AssetId, action: &str, target: MaskTarget) -> Value {
    let mut params = json!({"asset_id":asset_id,"action":action});
    let object = params.as_object_mut().expect("the envelope is an object");
    if let Some(mask) = target.mask {
        object.insert("mask".into(), json!(mask));
    }
    if let Some(component) = target.component {
        object.insert("component".into(), json!(component));
    }
    params
}

/// One `draft.set` and the one preview job for the settings it accepted, as a single round trip.
/// The gesture's bound is one of these per tick, so pairing them here is what keeps a preview from
/// being requested for settings the core never accepted.
///
/// The job asks for the reduction too. The contract requires the counts and the overlays to
/// describe the image currently presented, drafts included, and a drafted preview renders the whole
/// stack — so the worker that produced those pixels reduces them, exactly as it does for a
/// committed frame. A gesture therefore still costs one `draft.set` and one preview job per tick:
/// the analysis rides the job it already asked for and no second render happens.
///
/// It runs on the calling thread, synchronously: the owner's share is two `O(layers)` requests
/// that measure well under a millisecond, while handing the answer back through the runtime costs
/// a whole display frame whenever a redraw is in flight, which during a drag is always. A gesture
/// therefore pays the round trip where it is cheapest instead of waiting a frame for its result.
///
/// Both drafting gestures send through here: a slider's and a mask shape's. The mask overlay's
/// coverage grid is not asked for here, because [`crate::app::Editor::request_preview`] attaches
/// it to every job it queues, this one included — one rule for every preview path, so the grid is
/// requested once.
pub(crate) fn draft_set_now(
    owner: &OwnerHandle,
    client: ClientId,
    draft_id: DraftId,
    asset_id: AssetId,
    fields: Value,
    proxy: Option<ProxyBounds>,
) -> Result<(Draft, PreviewJob, RoundTrip), String> {
    let queued = Instant::now();
    let started = queued;
    let (draft, _) = call(
        owner,
        client,
        "draft.set",
        json!({"draft_id":draft_id,"fields":fields}),
    )?;
    let answered = Instant::now();
    let draft = parse::<Draft>(draft)?;
    let job = plan_preview(
        owner,
        proxied(
            PreviewRequest::new(client, asset_id)
                .draft(draft_id)
                .analyse(),
            proxy,
        ),
    )
    .map_err(|error| error.to_string())?;
    let planned = Instant::now();
    Ok((
        draft,
        job,
        RoundTrip {
            queued,
            started,
            answered,
            planned,
        },
    ))
}

/// Where the time of one `draft.set` round trip went, so an evidence run can tell the executor's
/// scheduling from the owner's own work.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RoundTrip {
    /// The task was created in `update`.
    pub(crate) queued: Instant,
    /// The task began running on the executor.
    pub(crate) started: Instant,
    /// The owner answered `draft.set`.
    pub(crate) answered: Instant,
    /// The owner returned the preview job.
    pub(crate) planned: Instant,
}

impl RoundTrip {
    /// The four legs in milliseconds: executor wait, `draft.set` on the owner, the preview job on
    /// the owner, and the return to `update` measured against `now`.
    pub(crate) fn legs_ms(&self, now: Instant) -> [f64; 4] {
        let ms = |from: Instant, to: Instant| to.duration_since(from).as_secs_f64() * 1000.0;
        [
            ms(self.queued, self.started),
            ms(self.started, self.answered),
            ms(self.answered, self.planned),
            ms(self.planned, now),
        ]
    }
}

/// Commit the draft once, as the plain call its task runs. A real outcome is read back exactly as
/// any other command's is; a no-op outcome created no entry, so nothing is refreshed and the gesture
/// simply ends.
///
/// The answer is read as a `mask.*` command's, which is the mutation envelope and, for a drafted
/// host command, what it changed — the history label, the mask, the component. A module action's
/// answer is the envelope alone, which that shape reads too; reading a mask command's as the bare
/// envelope would refuse its extra fields by name and lose a commit the core had already made.
pub(crate) fn draft_commit_now(
    owner: &OwnerHandle,
    client: ClientId,
    draft_id: &DraftId,
    asset_id: AssetId,
    mutation: Mutation,
    proxy: Option<ProxyBounds>,
) -> Result<Option<Refresh>, String> {
    let (committed, request) = call_own(
        owner,
        client,
        "draft.commit",
        json!({"draft_id":draft_id,"mutation":mutation}),
    )?;
    let result = parse::<ActionResult>(committed)?;
    if result.mutation.outcome == MutationOutcome::NoOp {
        return Ok(None);
    }
    let scope = Scope::Commit(result.mutation.revision);
    let mut refreshed = refresh(owner, client, asset_id, scope, proxy)?;
    refreshed.request = Some(request);
    Ok(Some(refreshed))
}

pub(crate) fn draft_commit_task(
    owner: OwnerHandle,
    client: ClientId,
    gesture: GestureId,
    draft_id: DraftId,
    asset_id: AssetId,
    mutation: Mutation,
    proxy: Option<ProxyBounds>,
) -> Task<Message> {
    let draft = draft_id.clone();
    Task::perform(
        async move { draft_commit_now(&owner, client, &draft_id, asset_id, mutation, proxy) },
        move |result| {
            Message::Draft(DraftMessage::Committed {
                gesture,
                draft,
                result: result.map(|refresh| refresh.map(Box::new)),
            })
        },
    )
}

/// What a cancel reads back after it: the displayed entry's own preview, at these bounds.
pub(crate) type Reseed = (AssetId, Option<EntryId>, Option<ProxyBounds>);

/// What a cancel answers: whether the owner ended the draft, and the frame read back after it.
pub(crate) type Cancelled = (
    Result<(), String>,
    Option<Result<Box<PreviewPayload>, String>>,
);

/// End the draft, then read back what the screen needs, as the plain calls the task runs. The
/// session is read **after** the cancel in the same task, so the one the desktop adopts can no
/// longer hold the draft; a separate task could read it first and put the ended draft back.
pub(crate) fn draft_cancel_now(
    owner: &OwnerHandle,
    client: ClientId,
    draft_id: &DraftId,
    reseed: Option<Reseed>,
) -> Cancelled {
    let cancelled = call(owner, client, "draft.cancel", json!({"draft_id":draft_id})).map(|_| ());
    let reseed = reseed.map(|(asset_id, entry_id, proxy)| {
        current_preview(owner, client, asset_id, entry_id, proxy).map(Box::new)
    });
    (cancelled, reseed)
}

pub(crate) fn draft_cancel_task(
    owner: OwnerHandle,
    client: ClientId,
    draft_id: DraftId,
    reseed: Option<Reseed>,
) -> Task<Message> {
    let draft = draft_id.clone();
    Task::perform(
        async move { draft_cancel_now(&owner, client, &draft_id, reseed) },
        move |(cancelled, reseed)| {
            Message::Draft(DraftMessage::Cancelled {
                draft,
                cancelled,
                reseed,
            })
        },
    )
}

/// Rebase the draft on the current revision, as the plain call its task runs.
pub(crate) fn draft_reapply_now(
    owner: &OwnerHandle,
    client: ClientId,
    draft_id: &DraftId,
) -> Result<Draft, String> {
    let (draft, _) = call(owner, client, "draft.reapply", json!({"draft_id":draft_id}))?;
    parse::<Draft>(draft)
}

pub(crate) fn draft_reapply_task(
    owner: OwnerHandle,
    client: ClientId,
    gesture: GestureId,
    draft_id: DraftId,
) -> Task<Message> {
    let draft = draft_id.clone();
    Task::perform(
        async move { draft_reapply_now(&owner, client, &draft_id) },
        move |result| {
            Message::Draft(DraftMessage::Reapplied {
                gesture,
                draft,
                result: result.map(Box::new),
            })
        },
    )
}

/// The displayed entry's own preview again, without a draft: what the canvas must show once a
/// gesture ended without committing. One preview job and one session read, no state or history
/// request and no history refresh. It is a displayed target, so its own worker reduces it and the
/// inspector follows the committed pixels back rather than emptying itself.
pub(crate) fn current_preview_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    entry_id: Option<EntryId>,
    proxy: Option<ProxyBounds>,
) -> Task<Message> {
    Task::perform(
        async move { current_preview(&owner, client, asset_id, entry_id, proxy) },
        |result| Message::Preview(PreviewMessage::Loaded(result.map(Box::new))),
    )
}

/// The plain calls [`current_preview_task`] runs.
fn current_preview(
    owner: &OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    entry_id: Option<EntryId>,
    proxy: Option<ProxyBounds>,
) -> Result<PreviewPayload, String> {
    let job = plan_preview(
        owner,
        proxied(
            PreviewRequest::new(client, asset_id)
                .entry(entry_id)
                .analyse(),
            proxy,
        ),
    )
    .map_err(|error| error.to_string())?;
    let (session, _) = call(owner, client, "session.state", json!({}))?;
    Ok(PreviewPayload {
        job,
        session: parse::<ClientSession>(session)?,
    })
}

/// The geometry tail of the displayed stack as one affine, read once when a mask gesture opens.
/// Every later pointer position is mapped from it locally, so a drag costs no host call per move.
/// The answer names the gesture that asked, so a map is never given to another one.
pub(crate) fn transform_task(
    owner: OwnerHandle,
    client: ClientId,
    gesture: GestureId,
    asset_id: AssetId,
    entry_id: Option<EntryId>,
) -> Task<Message> {
    Task::perform(
        async move {
            let (transform, _) = call(
                &owner,
                client,
                "render.transform",
                json!({"asset_id":asset_id,"entry_id":entry_id}),
            )?;
            parse::<StageTransform>(transform)
        },
        move |result| Message::Mask(MaskMessage::Transform(gesture, result)),
    )
}

pub(crate) fn session_task(
    owner: OwnerHandle,
    client: ClientId,
    method: &'static str,
    params: Value,
) -> Task<Message> {
    Task::perform(
        async move {
            let (result, _) = call(&owner, client, method, params)?;
            parse::<ClientSession>(result)
        },
        |value| Message::View(ViewMessage::SessionUpdated(value)),
    )
}

/// Panel collapse, canvas mode and the thirds overlay are per-client session state the owner holds;
/// the desktop keeps no copy outside the session it adopts back.
pub(crate) fn workspace_task(owner: OwnerHandle, client: ClientId, params: Value) -> Task<Message> {
    Task::perform(
        async move {
            let (result, _) = call(&owner, client, "workspace.set", params)?;
            parse::<ClientSession>(result)
        },
        |value| Message::View(ViewMessage::WorkspaceUpdated(value)),
    )
}

/// Where one picked view pixel lands in the content stage. This is `render.locate`, the same method
/// an API client calls, so the canvas and the API share one mapping and the desktop holds none of
/// it. It reads only, costs `O(layers)` in the core and rasterizes nothing, so it runs off the
/// update loop like every other owner call and no pick blocks the pointer.
pub(crate) fn locate_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    entry: EntryId,
    mode: String,
    x: u32,
    y: u32,
) -> Task<Message> {
    let picked = entry.clone();
    let picked_mode = mode.clone();
    Task::perform(
        async move {
            let (located, _) = call(
                &owner,
                client,
                "render.locate",
                json!({"asset_id":asset_id,"entry_id":entry,"x":x,"y":y}),
            )?;
            parse::<ContentPoint>(located)
        },
        move |result| {
            Message::Pointer(PointerMessage::Located {
                entry: picked.clone(),
                mode: picked_mode.clone(),
                view: (x, y),
                result,
            })
        },
    )
}

/// One declared read-only query at a located content pixel, which is what a `sample-apply` canvas
/// mode asks before it commits anything.
///
/// `method` is the whole method name, because the two kinds of pick read two namespaces: a module's
/// is `query.<id>` and the host's own is a `mask.*` read. Either way it mutates nothing, writes no
/// history entry and emits no event, and the core answers it from point samples over the compiled
/// evaluation a commit would plan against, so a pick renders no frame. The coordinate parameter names
/// and the extra envelope fields — the mask a host pick addresses — come from the declaration and from
/// the panel's own selection, not from this file.
#[allow(clippy::too_many_arguments)]
pub(crate) fn query_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    entry: EntryId,
    method: String,
    action: String,
    coordinates: (String, String),
    envelope: serde_json::Map<String, Value>,
    point: (u32, u32),
) -> Task<Message> {
    let answered = entry.clone();
    Task::perform(
        async move {
            let mut params = json!({"asset_id":asset_id,"entry_id":entry});
            let object = params.as_object_mut().expect("the envelope is an object");
            for (name, value) in envelope {
                object.insert(name, value);
            }
            object.insert(coordinates.0, Value::from(point.0));
            object.insert(coordinates.1, Value::from(point.1));
            call(&owner, client, &method, params).map(|(value, _)| value)
        },
        move |result| {
            Message::Pointer(PointerMessage::SampleQueried {
                entry: answered.clone(),
                action: action.clone(),
                point,
                result,
            })
        },
    )
}

/// One pixel of the displayed stack for the pointer readout. It is a point query: `render.sample`
/// evaluates the compiled recipe at one coordinate and rasterizes nothing, so hovering costs
/// O(layers) on the owner thread and never a frame. The entry travels back with the answer, so a
/// response that arrives after the canvas moved to another stack is dropped rather than shown.
pub(crate) fn sample_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    entry: EntryId,
    draft_id: Option<String>,
    x: u32,
    y: u32,
) -> Task<Message> {
    let sampled = entry.clone();
    Task::perform(
        async move {
            let mut params = json!({"asset_id":asset_id,"x":x,"y":y});
            if let Some(draft_id) = draft_id
                && let Some(object) = params.as_object_mut()
            {
                object.insert("draft_id".into(), Value::from(draft_id));
            }
            let (sampled, _) = call(&owner, client, "render.sample", params)?;
            let rgba: Option<[u8; 4]> = parse(sampled["rgba"].clone())?;
            // Outside the output stage is not an error: the pointer simply has nothing under it.
            rgba.map(|rgba| Readout { x, y, rgba })
                .ok_or_else(|| format!("({x}, {y}) is outside the rendered image"))
        },
        move |result| {
            Message::Pointer(PointerMessage::Sampled {
                entry: sampled.clone(),
                result,
            })
        },
    )
}

pub(crate) fn pan_task(owner: OwnerHandle, client: ClientId, x: f32, y: f32) -> Task<Message> {
    Task::perform(
        async move {
            let (result, _) = call(&owner, client, "view.set", json!({"pan_x":x,"pan_y":y}))?;
            parse::<ClientSession>(result)
        },
        |value| Message::View(ViewMessage::PanSynced(value)),
    )
}

pub(crate) fn versions_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    method: &'static str,
    params: Value,
) -> Task<Message> {
    Task::perform(
        async move {
            let (_, request) = call_own(&owner, client, method, params)?;
            let (mut listed, _) =
                call(&owner, client, "version.list", json!({"asset_id":asset_id}))?;
            Ok((parse::<Vec<Version>>(listed["versions"].take())?, request))
        },
        |value| Message::History(HistoryMessage::VersionsLoaded(value)),
    )
}

pub(crate) fn sync_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    after: u64,
    own: Vec<String>,
    proxy: Option<ProxyBounds>,
) -> Task<Message> {
    Task::perform(
        async move { sync_now(&owner, client, asset_id, after, &own, proxy) },
        |value| Message::Sync(SyncMessage::Synced(value)),
    )
}

/// One read by the Performance section's sampler: the counters, then the activity board, as the
/// owner answered them.
#[derive(Clone, Debug)]
pub(crate) struct PerformanceRead {
    pub(crate) resources: Value,
    pub(crate) activity: Value,
    /// Milliseconds since the Unix epoch, taken the moment `resources.read` answered, so evidence
    /// can place the sample beside a reading of this process that another program took.
    pub(crate) wall_ms: u64,
}

/// One sampler read off the UI thread: `resources.read` and then `activity.list`, through the same
/// method table as any API client, answered as one message tagged with the sampling epoch that
/// asked for it. Neither method mutates anything or emits an event, so a sampling section never
/// makes this or any other client resynchronise.
pub(crate) fn performance_task(owner: OwnerHandle, client: ClientId, epoch: u64) -> Task<Message> {
    Task::perform(
        async move { read_performance(&owner, client) },
        move |result| {
            Message::Performance(PerformanceMessage::Sampled {
                epoch,
                result: result.map(Box::new),
            })
        },
    )
}

fn read_performance(owner: &OwnerHandle, client: ClientId) -> Result<PerformanceRead, String> {
    let (resources, _) = call(owner, client, "resources.read", json!({}))?;
    let wall_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| u64::try_from(since.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default();
    let (activity, _) = call(owner, client, "activity.list", json!({}))?;
    Ok(PerformanceRead {
        resources,
        activity,
        wall_ms,
    })
}

/// A method whose event changes the preset library rather than an asset.
fn is_library_event(method: &str) -> bool {
    method.starts_with("preset.")
}

/// One poll: the events since `after`, then only the reads they call for. A poll that saw nothing
/// reads nothing else, and a preset event from another client costs one `preset.list` and no asset
/// refresh or preview.
///
/// `own` names this desktop's requests whose answers already read their changes back and reached
/// the screen. Their events are read and skipped, so a commit of this desktop's own costs its poll
/// nothing; any other event, including one of this desktop's whose read-back never arrived, is
/// read back here. Every read is made after `events.since` answered, so it covers every event up to
/// the sequence the poll returns.
pub(crate) fn sync_now(
    owner: &OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    after: u64,
    own: &[String],
    proxy: Option<ProxyBounds>,
) -> Result<SyncResult, String> {
    let (events, _) = call(owner, client, "events.since", json!({"after":after}))?;
    let mut events: EventsResult = parse(events)?;
    let sequence = events.current_sequence;
    let (read, others): (Vec<_>, Vec<_>) = std::mem::take(&mut events.events)
        .into_iter()
        .partition(|event| own.contains(&event.request_id));
    events.events = others;
    let library = events.gap
        || events
            .events
            .iter()
            .any(|event| is_library_event(&event.method));
    // A capability event changes a module, never the asset, so it alone renders nothing.
    let capabilities = events.gap
        || events
            .events
            .iter()
            .any(|event| capability_event(&event.method));
    let asset = events.gap
        || events
            .events
            .iter()
            .any(|event| !is_library_event(&event.method) && !capability_event(&event.method));
    let presets = if library {
        Some(list_presets(owner, client)?)
    } else {
        None
    };
    let refresh = if asset {
        Some(Box::new(refresh(
            owner,
            client,
            asset_id,
            Scope::Elsewhere,
            proxy,
        )?))
    } else {
        None
    };
    Ok(SyncResult {
        sequence,
        refresh,
        presets,
        capabilities,
        own: read.into_iter().map(|event| event.request_id).collect(),
    })
}

/// The whole library, as `preset.list` lists it: record reads only, no render and no source.
pub(crate) fn list_presets(
    owner: &OwnerHandle,
    client: ClientId,
) -> Result<(Vec<PresetSummary>, u64), String> {
    let (mut listed, sequence) = call(owner, client, "preset.list", json!({}))?;
    Ok((parse(listed["presets"].take())?, sequence))
}

/// Load the library, at startup and whenever this desktop needs the listing again.
pub(crate) fn presets_task(owner: OwnerHandle, client: ClientId) -> Task<Message> {
    Task::perform(async move { list_presets(&owner, client) }, |result| {
        Message::Preset(PresetMessage::Listed(result))
    })
}

/// One library method and the listing after it.
fn preset_change(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<PresetChange, String> {
    let (result, request) = call_own(owner, client, method, params)?;
    let (presets, sequence) = list_presets(owner, client)?;
    Ok(PresetChange {
        result,
        presets,
        sequence,
        request,
    })
}

/// Capture the checked fields from one entry, store them as a preset and list the library, as one
/// task: `capture` is the whole `preset.capture` request and `create` the `preset.create` request
/// the captured settings complete.
pub(crate) fn preset_create_now(
    owner: &OwnerHandle,
    client: ClientId,
    capture: Value,
    mut create: Value,
) -> Result<PresetChange, String> {
    let (mut captured, _) = call(owner, client, "preset.capture", capture)?;
    create["settings"] = captured["settings"].take();
    preset_change(owner, client, "preset.create", create)
}

pub(crate) fn preset_create_task(
    owner: OwnerHandle,
    client: ClientId,
    capture: Value,
    create: Value,
) -> Task<Message> {
    Task::perform(
        async move { preset_create_now(&owner, client, capture, create) },
        |result| Message::Preset(PresetMessage::Created(result.map(Box::new))),
    )
}

/// Read one chosen preset file as text, on the task's thread. A file larger than the importer
/// accepts is refused from its length before a byte is read, the read itself is bounded in case the
/// file grows meanwhile, and text that is not UTF-8 is refused rather than guessed at.
pub(crate) fn read_preset_file(path: &Path) -> Result<String, String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let too_large = |length: u64| {
        format!(
            "{}: {name} is {length} bytes; a preset file is at most 1 MiB ({MAX_PRESET_BYTES} bytes)",
            ErrorKind::ResourceLimit.code()
        )
    };
    let unreadable = |error: std::io::Error| {
        format!(
            "{}: cannot read {name}: {error}",
            ErrorKind::FileAccess.code()
        )
    };
    let file = std::fs::File::open(path).map_err(unreadable)?;
    let length = file.metadata().map_err(unreadable)?.len();
    if length > MAX_PRESET_BYTES as u64 {
        return Err(too_large(length));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_PRESET_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() > MAX_PRESET_BYTES {
        return Err(too_large(bytes.len() as u64));
    }
    String::from_utf8(bytes).map_err(|_| {
        format!(
            "{}: {name} is not UTF-8 text",
            ErrorKind::UnsupportedInput.code()
        )
    })
}

/// Import one preset file: read it here, send its text and name to `preset.import`, and list the
/// library. The owner thread never touches the file.
pub(crate) fn preset_import_now(
    owner: &OwnerHandle,
    client: ClientId,
    path: &Path,
) -> Result<PresetChange, String> {
    let content = read_preset_file(path)?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    preset_change(
        owner,
        client,
        "preset.import",
        json!({"content": content, "file_name": file_name, "mutation": request()}),
    )
}

pub(crate) fn preset_import_task(
    owner: OwnerHandle,
    client: ClientId,
    path: PathBuf,
) -> Task<Message> {
    Task::perform(
        async move { preset_import_now(&owner, client, &path) },
        |result| Message::Preset(PresetMessage::Imported(result.map(Box::new))),
    )
}

pub(crate) fn preset_delete_now(
    owner: &OwnerHandle,
    client: ClientId,
    id: &str,
) -> Result<PresetChange, String> {
    preset_change(
        owner,
        client,
        "preset.delete",
        json!({"preset_id": id, "mutation": request()}),
    )
}

pub(crate) fn preset_delete_task(
    owner: OwnerHandle,
    client: ClientId,
    id: String,
) -> Task<Message> {
    Task::perform(
        async move { preset_delete_now(&owner, client, &id) },
        |result| Message::Preset(PresetMessage::Deleted(result.map(Box::new))),
    )
}

/// One imported preset's whole import report, as the text Copy puts on the clipboard.
pub(crate) fn preset_report_task(
    owner: OwnerHandle,
    client: ClientId,
    id: String,
) -> Task<Message> {
    Task::perform(
        async move {
            let (read, _) = call(&owner, client, "preset.read", json!({"preset_id": id}))?;
            serde_json::to_string_pretty(&read["preset"]["report"])
                .map_err(|error| error.to_string())
        },
        |result| Message::Preset(PresetMessage::ReportRead(result)),
    )
}

/// Export one preset: `preset.export` writes the document, the native save dialog chooses where,
/// suggesting the document's own file name, and the file is written here once the dialog has
/// answered. Replacing an existing file is the dialog's own question; nothing is written without
/// it, and a cancelled dialog writes nothing.
pub(crate) fn preset_export_task(
    owner: OwnerHandle,
    client: ClientId,
    id: String,
) -> Task<Message> {
    Task::perform(
        async move {
            let (exported, _) = call(&owner, client, "preset.export", json!({"preset_id": id}))?;
            let file_name = exported["file_name"]
                .as_str()
                .ok_or("preset.export returned no file name")?
                .to_owned();
            let content = exported["content"]
                .as_str()
                .ok_or("preset.export returned no content")?
                .to_owned();
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_file_name(&file_name)
                .add_filter("Lightwell preset", &["lwpreset"])
                .save_file()
                .await
            else {
                return Ok(None);
            };
            std::fs::write(file.path(), content).map_err(|error| {
                format!(
                    "{}: cannot write {}: {error}",
                    ErrorKind::FileAccess.code(),
                    file.file_name()
                )
            })?;
            Ok(Some(file.file_name()))
        },
        |result| Message::Preset(PresetMessage::Exported(result)),
    )
}

/// One host method as an evidence script names it, with the library listed after it when the
/// method is one of the library's own, exactly as the section refreshes after its own calls.
pub(crate) fn host_task(
    owner: OwnerHandle,
    client: ClientId,
    method: String,
    params: Value,
) -> Task<Message> {
    Task::perform(
        async move {
            let (result, sequence) = call(&owner, client, &method, params)?;
            let (presets, sequence) = if is_library_event(&method) {
                let (presets, seen) = list_presets(&owner, client)?;
                (Some(presets), sequence.max(seen))
            } else {
                (None, sequence)
            };
            Ok(HostAnswer {
                method,
                result,
                presets,
                sequence,
            })
        },
        |result| Message::Evidence(EvidenceMessage::HostAnswered(result.map(Box::new))),
    )
}

pub(crate) fn older_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    before_sequence: u64,
) -> Task<Message> {
    Task::perform(
        async move {
            let (page, _) = call(
                &owner,
                client,
                "history.list",
                json!({"asset_id":asset_id,"before_sequence":before_sequence,"limit":HISTORY_PAGE_SIZE}),
            )?;
            parse::<HistoryPage>(page)
        },
        |value| Message::History(HistoryMessage::OlderLoaded(value)),
    )
}

/// Merge one entry's row into the loaded page, newest first and bounded to one page: what a commit,
/// an undo, a redo and a restore of this desktop's own do in place of reading the page again, as
/// performance rule 7 asks.
pub(crate) fn merge_current_entry(history: &mut HistoryPage, entry: HistoryRow) {
    history.entries.retain(|existing| existing.id != entry.id);
    let position = history
        .entries
        .partition_point(|existing| existing.sequence > entry.sequence);
    history.entries.insert(position, entry);
    history.entries.truncate(HISTORY_PAGE_SIZE);
    history.next_before_sequence = (history.entries.len() == HISTORY_PAGE_SIZE)
        .then(|| history.entries.last().expect("page is not empty").sequence);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::testing::entry;

    /// A real owner with the S0 fixture imported, and the refresh the open task reads for it.
    struct Opened {
        owner: OwnerHandle,
        join: std::thread::JoinHandle<()>,
        catalog: PathBuf,
        client: ClientId,
        asset: AssetId,
        refresh: Refresh,
    }

    impl Opened {
        /// The plain calls [`import_task`] runs, with the calls of its refresh recorded.
        fn new() -> (Self, Vec<String>) {
            let catalog = std::env::temp_dir().join(format!(
                "lightwell-refresh-scope-{}-{}.sqlite",
                std::process::id(),
                REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
            ));
            let (owner, join) = OwnerHandle::start(&catalog).unwrap();
            let client = owner.register();
            let fixture =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
            let (queued, _) = call(
                &owner,
                client,
                "catalog.import",
                json!({"path": fixture, "mutation": request()}),
            )
            .unwrap();
            let job = queued["job_id"].as_str().unwrap().to_owned();
            let state = wait_source_job(&owner, client, &job, None).unwrap();
            call(&owner, client, "job.adopt", json!({"job_id": job})).unwrap();
            let asset = state.asset.id;
            owner_calls::take();
            let refresh = refresh(&owner, client, asset.clone(), Scope::Open, None).unwrap();
            let calls = owner_calls::take();
            let opened = Self {
                owner,
                join,
                catalog,
                client,
                asset,
                refresh,
            };
            (opened, calls)
        }

        /// One command of the desktop's own, as [`state_task`] runs it, and the owner calls it made.
        fn command(&mut self, method: &str, mut params: Value) -> Vec<String> {
            params["asset_id"] = json!(self.asset);
            params["mutation"] = json!(mutation(self.refresh.state.revision));
            owner_calls::take();
            self.refresh = command_now(
                &self.owner,
                self.client,
                self.asset.clone(),
                method,
                params,
                None,
            )
            .unwrap_or_else(|error| panic!("{method}: {error}"));
            owner_calls::take()
        }

        fn finish(self) {
            self.owner.stop();
            self.join.join().unwrap();
            std::fs::remove_file(self.catalog).unwrap();
        }
    }

    /// The owner calls each completion path makes, counted at the call helper: a commit reads only
    /// what its panels show and one preview job, undo, redo and restore add the lineage, and only
    /// the open reads the page, the versions and the Original — which it finds on that page.
    #[test]
    fn a_commit_an_undo_and_a_restore_read_back_only_what_they_changed() {
        let (mut opened, open) = Opened::new();
        assert_eq!(
            open,
            [
                "asset.state",
                "history.list",
                "version.list",
                "history.lineage",
                "session.state",
                "recipe.describe",
                "mask.list",
                "preview_job",
            ],
            "an open finds the Original on the page it read"
        );
        let original = opened.refresh.state.current_entry.id.clone();
        assert_eq!(opened.refresh.original, Some(original.clone()));

        let commit = opened.command("edit.transform", json!({"transform": "rotate-left"}));
        assert_eq!(
            commit,
            [
                "edit.transform",
                "asset.state",
                "session.state",
                "recipe.describe",
                "mask.list",
                "preview_job",
            ]
        );
        let refreshed = &opened.refresh;
        assert!(refreshed.history.is_none() && refreshed.versions.is_none());
        assert!(refreshed.lineage.is_none() && refreshed.original.is_none());
        assert_eq!(refreshed.job.entry.id, refreshed.state.current_entry.id);
        assert_eq!(refreshed.recipe.entry_id, refreshed.state.current_entry.id);

        let navigated = [
            "asset.state",
            "history.lineage",
            "session.state",
            "recipe.describe",
            "mask.list",
            "preview_job",
        ];
        let undo = opened.command("history.undo", json!({}));
        assert_eq!(undo[0], "history.undo");
        assert_eq!(undo[1..], navigated);
        assert_eq!(opened.refresh.state.current_entry.id, original);
        let redo = opened.command("history.redo", json!({}));
        assert_eq!(redo[0], "history.redo");
        assert_eq!(redo[1..], navigated);
        let restore = opened.command("history.restore", json!({"entry_id": original}));
        assert_eq!(restore[0], "history.restore");
        assert_eq!(restore[1..], navigated);
        let lineage = opened
            .refresh
            .lineage
            .as_ref()
            .expect("a restore reads the lineage");
        assert_eq!(
            lineage.steps[0].entry_id,
            opened.refresh.state.current_entry.id
        );
        opened.finish();
    }

    /// A commit is merged on the desktop only when the state read is the one the commit left: when
    /// another client's commit landed in between, the refresh reads what an event from elsewhere
    /// reads, page and lineage included, so no entry goes missing from the loaded page.
    #[test]
    fn a_commit_overtaken_by_another_clients_reads_the_page() {
        let (opened, _) = Opened::new();
        let agent = opened.owner.register();
        let answered = opened.refresh.state.revision;
        call(
            &opened.owner,
            agent,
            "edit.transform",
            json!({"asset_id": opened.asset, "mutation": mutation(answered),
                   "transform": "rotate-right"}),
        )
        .unwrap();
        owner_calls::take();
        let read = refresh(
            &opened.owner,
            opened.client,
            opened.asset.clone(),
            Scope::Commit(answered),
            None,
        )
        .unwrap();
        assert_eq!(
            owner_calls::take(),
            [
                "asset.state",
                "history.list",
                "version.list",
                "history.lineage",
                "session.state",
                "recipe.describe",
                "mask.list",
                "preview_job",
            ]
        );
        assert_eq!(read.history.expect("the page").entries.len(), 2);
        assert!(
            read.original.is_none(),
            "the Original is read once, at open"
        );
        opened.finish();
    }

    /// The desktop through its own update, against a real owner: the poll's event cursor advances
    /// only past events a poll read. Another client's change that lands between two of the
    /// desktop's own calls — a version named, which moves no revision and so conflicts with nothing
    /// — is answered past by every response after it, and it still reaches the next poll, which
    /// reads it and the versions back. The desktop's own import and commits are read and skipped,
    /// and no answer moves the cursor backwards.
    #[test]
    fn the_event_sequence_advances_only_past_events_a_poll_read() {
        use crate::app::{Editor, message::Message, testing};
        let (mut editor, catalog) = testing::boot();
        let (owner, client) = (editor.owner.clone(), editor.client);
        let agent = owner.register();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg");
        let opened = import_now(&owner, client, &fixture, 0, &AtomicU64::new(0), None, None)
            .expect("the import opens");
        let asset = opened.state.asset.id.clone();
        let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(opened)))));
        assert_eq!(editor.api_sequence, 0, "an open reads no event");
        let mut cursor = editor.api_sequence;
        let mut poll = |editor: &mut Editor| {
            let own = editor.own_requests.iter().cloned().collect::<Vec<_>>();
            let polled = sync_now(
                &owner,
                client,
                asset.clone(),
                editor.api_sequence,
                &own,
                None,
            )
            .expect("the poll answers");
            let result = (polled.refresh.is_some(), polled.own.len());
            let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(polled))));
            assert!(editor.api_sequence >= cursor, "never backwards");
            cursor = editor.api_sequence;
            result
        };
        assert_eq!(
            poll(&mut editor),
            (false, 1),
            "the desktop's own import is read and costs nothing"
        );
        let caught_up = editor.api_sequence;

        let command = |editor: &mut Editor, transform: &str| {
            let revision = editor.state.as_ref().unwrap().revision;
            let refreshed = command_now(
                &editor.owner,
                client,
                asset.clone(),
                "edit.transform",
                json!({"asset_id": asset, "mutation": mutation(revision), "transform": transform}),
                None,
            )
            .expect("the command commits");
            let _ = editor.update(Message::Sync(SyncMessage::Refreshed(Ok(Box::new(
                refreshed,
            )))));
        };
        command(&mut editor, "rotate-left");
        call(
            &owner,
            agent,
            "version.create",
            json!({"asset_id": asset, "name": "Keep", "mutation": request()}),
        )
        .unwrap();
        command(&mut editor, "rotate-right");
        assert_eq!(
            editor.api_sequence, caught_up,
            "the commits' answers, which count the agent's event, move no cursor"
        );
        assert!(editor.versions.is_empty(), "no read so far saw the version");

        assert_eq!(
            poll(&mut editor),
            (true, 2),
            "the agent's event is read back; the two commits are skipped"
        );
        assert_eq!(
            editor
                .versions
                .iter()
                .map(|version| version.name.as_str())
                .collect::<Vec<_>>(),
            ["Keep"],
            "the version named between the two commits reached the screen"
        );
        assert_eq!(editor.api_sequence, caught_up + 3);
        assert!(
            editor.own_requests.is_empty(),
            "every skipped event is forgotten"
        );
        assert_eq!(
            poll(&mut editor),
            (false, 0),
            "and the next poll reads nothing"
        );

        // An answer read at an older sequence never moves the cursor back.
        let stale = SyncResult {
            sequence: caught_up,
            refresh: None,
            presets: None,
            capabilities: false,
            own: Vec::new(),
        };
        let _ = editor.update(Message::Sync(SyncMessage::Synced(Ok(stale))));
        assert_eq!(editor.api_sequence, caught_up + 3);
        testing::finish(editor, catalog);
    }

    #[test]
    fn current_entry_merge_is_newest_first_and_bounded() {
        let asset = AssetId::new();
        let row = |sequence: u64| HistoryRow::from(&entry(&asset, sequence, None));
        let mut history = HistoryPage {
            entries: (0..HISTORY_PAGE_SIZE as u64).rev().map(row).collect(),
            next_before_sequence: None,
        };
        merge_current_entry(&mut history, row(HISTORY_PAGE_SIZE as u64));
        assert_eq!(history.entries.len(), HISTORY_PAGE_SIZE);
        assert_eq!(history.entries[0].sequence, HISTORY_PAGE_SIZE as u64);
        assert_eq!(history.entries.last().unwrap().sequence, 1);
        assert_eq!(history.next_before_sequence, Some(1));
    }
}
