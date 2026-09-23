//! The owner tasks. Every desktop request goes through `call`, which is the same method table the
//! JSON API dispatches; there is no desktop-only mutation path. Each task takes the narrowest
//! completion path the performance rules allow.
use crate::{
    app::message::{Message, PresetMessage},
    state::histogram::Readout,
};
use iced::Task;
use lightwell_core::{
    ApiRequest, AssetId, ClientId, ClientSession, ContentPoint, Draft, DraftId, EditorState,
    EntryId, ErrorKind, EventsResult, HistoryEntry, HistoryPage, HistorySelection, Lineage,
    MAX_PRESET_BYTES, MaskOverlayRequest, ModuleDescriptor, Mutation, MutationOutcome,
    MutationResult, OwnerHandle, PresetSummary, PreviewJob, PreviewRequest, ProxyBounds,
    RecipeDescription, StageTransform, Version,
    mask::commands::{MaskCommandResult, MaskListing, MaskTarget},
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

/// Authoritative state read back from the owner after a change. `history` is `None` when only the
/// current entry needs merging into the loaded page.
#[derive(Clone, Debug)]
pub(crate) struct Refresh {
    pub(crate) state: EditorState,
    pub(crate) history: Option<HistoryPage>,
    pub(crate) versions: Vec<Version>,
    pub(crate) lineage: Lineage,
    /// The displayed entry's layers as the recipe panel reads them.
    pub(crate) recipe: RecipeDescription,
    /// The same entry's masks. It is read beside the recipe and never on its own, so the panel can
    /// never show a mask list and a layer list that describe two different entries.
    pub(crate) masks: MaskListing,
    /// The Original entry, looked up once per asset so Compare needs no search.
    pub(crate) original: Option<EntryId>,
    pub(crate) job: PreviewJob,
    pub(crate) session: ClientSession,
    pub(crate) sequence: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct PreviewPayload {
    pub(crate) job: PreviewJob,
    pub(crate) session: ClientSession,
    pub(crate) sequence: u64,
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
    /// Why these pixels are being uploaded when no render asked for it: `Some("zoom")` is the
    /// retained raster a zoom change needed. `None` is the ordinary path, a rendered frame.
    pub(crate) reason: Option<&'static str>,
}

/// What one live-refresh poll found. One poll answers both kinds of change: the asset's state is
/// read back when any event other than a preset one arrived, the preset library when a `preset.*`
/// event did, and both after a gap in the log, which could have hidden either.
#[derive(Clone, Debug)]
pub(crate) struct SyncResult {
    pub(crate) sequence: u64,
    pub(crate) refresh: Option<Box<Refresh>>,
    /// The listing and the event sequence it was read at.
    pub(crate) presets: Option<(Vec<PresetSummary>, u64)>,
}

#[cfg(test)]
impl SyncResult {
    /// A poll that saw an asset event and read the state back.
    pub(crate) fn changed(refresh: Refresh) -> Self {
        Self {
            sequence: refresh.sequence,
            refresh: Some(Box::new(refresh)),
            presets: None,
        }
    }
}

/// One library call of this desktop's and the listing read right after it, so the section shows
/// the library the call left behind rather than the one before it.
#[derive(Clone, Debug)]
pub(crate) struct PresetChange {
    /// What the call itself answered.
    pub(crate) result: Value,
    pub(crate) presets: Vec<PresetSummary>,
    pub(crate) sequence: u64,
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

pub(crate) fn mutation(revision: u64) -> Mutation {
    Mutation {
        expected_revision: revision,
        request_id: format!(
            "desktop-{}-{}",
            std::process::id(),
            REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
        ),
        actor: ACTOR.into(),
    }
}

pub(crate) fn call(
    owner: &OwnerHandle,
    client: ClientId,
    method: &str,
    params: Value,
) -> Result<(Value, u64), String> {
    let request = ApiRequest {
        id: format!("ui-{}", REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)),
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
    preview_guard: Option<(&AssetId, &PreviewExpectation)>,
) -> Result<EditorState, String> {
    loop {
        if open_guard.is_some_and(|(guard, generation)| guard.load(Ordering::Acquire) != generation)
        {
            let _ = call(owner, client, "job.cancel", json!({"job_id":job_id}));
            return Err("superseded open".into());
        }
        if let Some((asset, expected)) = preview_guard
            && !preview_still_current(owner, client, asset, expected)?
        {
            // Other requests from this client may share this source flight. Let the bounded
            // worker finish once, but stop this obsolete caller from retrying or publishing it.
            return Err("superseded preview".into());
        }
        let (status, _) = call(owner, client, "job.status", json!({"job_id":job_id}))?;
        match status["state"].as_str() {
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
            Some("queued" | "preparing") => {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            _ => return Err("unexpected source job state".into()),
        }
    }
}

struct PreviewExpectation {
    session_generation: u64,
    entry: EntryId,
    current: bool,
}

fn preview_still_current(
    owner: &OwnerHandle,
    client: ClientId,
    asset_id: &AssetId,
    expected: &PreviewExpectation,
) -> Result<bool, String> {
    let (session, _) = call(owner, client, "session.state", json!({}))?;
    let session: ClientSession = parse(session)?;
    if session.preview.generation != expected.session_generation {
        return Ok(false);
    }
    if expected.current {
        let (state, _) = call(owner, client, "asset.state", json!({"asset_id":asset_id}))?;
        let state: EditorState = parse(state)?;
        Ok(state.current_entry.id == expected.entry)
    } else {
        Ok(
            matches!(session.preview.selection, HistorySelection::Entry(ref id) if *id == expected.entry),
        )
    }
}

fn ready_preview_job(
    owner: &OwnerHandle,
    request: PreviewRequest,
    expected: &PreviewExpectation,
) -> Result<PreviewJob, String> {
    let client = request.client;
    let asset_id = request.asset_id.clone();
    loop {
        if !preview_still_current(owner, client, &asset_id, expected)? {
            return Err("superseded preview".into());
        }
        match owner.preview_job(request.clone()) {
            Ok(job) => {
                if preview_still_current(owner, client, &asset_id, expected)? {
                    return Ok(job);
                }
                return Err("superseded preview".into());
            }
            Err(error) if error.kind == lightwell_core::ErrorKind::PreparationRequired => {
                let _ = wait_source_job(
                    owner,
                    client,
                    &error.detail,
                    None,
                    Some((&asset_id, expected)),
                )?;
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

/// Read authoritative state back after a change or an external event.
pub(crate) fn refresh(
    owner: &OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    with_history: bool,
    mut sequence: u64,
    proxy: Option<ProxyBounds>,
) -> Result<Refresh, String> {
    let mut fetch = |method: &str, params: Value| -> Result<Value, String> {
        let (value, seen) = call(owner, client, method, params)?;
        sequence = sequence.max(seen);
        Ok(value)
    };
    let state: EditorState = parse(fetch("asset.state", json!({"asset_id":asset_id}))?)?;
    let history = if with_history {
        Some(parse::<HistoryPage>(fetch(
            "history.list",
            json!({"asset_id":asset_id,"before_sequence":null,"limit":HISTORY_PAGE_SIZE}),
        )?)?)
    } else {
        None
    };
    let versions: Vec<Version> =
        parse(fetch("version.list", json!({"asset_id":asset_id}))?["versions"].take())?;
    let lineage: Lineage = parse(fetch(
        "history.lineage",
        json!({"asset_id":asset_id,"limit":LINEAGE_LIMIT}),
    )?)?;
    let session: ClientSession = parse(fetch("session.state", json!({}))?)?;
    let selected = match &session.preview.selection {
        HistorySelection::Current => None,
        HistorySelection::Entry(entry_id) => Some(entry_id.clone()),
    };
    // The recipe rows of the entry that will be displayed: O(layers) payload reads, no render.
    let recipe: RecipeDescription = parse(fetch(
        "recipe.describe",
        json!({"asset_id":asset_id,"entry_id":selected}),
    )?)?;
    // The masks of that same entry. `recipe.describe` names each layer's mask and `mask.list` names
    // each mask's layers, so reading both together is what lets the panel show the relation from
    // either side without a second round trip.
    let masks: MaskListing = parse(fetch("mask.list", mask_list_params(&asset_id, &selected))?)?;
    // The Original entry is sequence 0, so one bounded page before sequence 1 finds it.
    let original: HistoryPage = parse(fetch(
        "history.list",
        json!({"asset_id":asset_id,"before_sequence":1,"limit":1}),
    )?)?;
    let expected = PreviewExpectation {
        session_generation: session.preview.generation,
        entry: selected
            .clone()
            .unwrap_or_else(|| state.current_entry.id.clone()),
        current: selected.is_none(),
    };
    // Every preview of the displayed target is reduced by the same worker that rendered it, so
    // the histogram needs no second render and an `analysis.request` for this identity is a
    // cache hit. A truncated crop-draft job is the one exception; the core refuses to analyse
    // it, because its identity describes the whole stack rather than the prefix it renders.
    let job = ready_preview_job(
        owner,
        proxied(
            PreviewRequest::new(client, asset_id)
                .entry(selected)
                .analyse(),
            proxy,
        ),
        &expected,
    )?;
    Ok(Refresh {
        state,
        history,
        versions,
        lineage,
        recipe,
        masks,
        original: original.entries.first().map(|entry| entry.id.clone()),
        job,
        session,
        sequence,
    })
}

/// Discovery runs once: the controls on screen are whatever the registered modules declare.
pub(crate) fn modules_task(owner: OwnerHandle, client: ClientId) -> Task<Message> {
    Task::perform(
        async move {
            let (mut listed, _) = call(&owner, client, "module.list", json!({}))?;
            parse::<Vec<ModuleDescriptor>>(listed["modules"].take())
        },
        Message::ModulesLoaded,
    )
}

pub(crate) fn import_task(
    owner: OwnerHandle,
    client: ClientId,
    path: PathBuf,
    generation: u64,
    open_guard: Arc<AtomicU64>,
    proxy: Option<ProxyBounds>,
) -> Task<Message> {
    Task::perform(
        async move {
            let (result, sequence) = call(&owner, client, "catalog.import", json!({"path":path}))?;
            let job_id = result["job_id"]
                .as_str()
                .ok_or("catalog.import did not return a source job")?;
            let state = wait_source_job(
                &owner,
                client,
                job_id,
                Some((&open_guard, generation)),
                None,
            )?;
            if open_guard.load(Ordering::Acquire) != generation {
                let _ = call(&owner, client, "job.cancel", json!({"job_id":job_id}));
                return Err("superseded open".into());
            }
            let (_, adopted_sequence) =
                call(&owner, client, "job.adopt", json!({"job_id":job_id}))?;
            let refreshed = refresh(
                &owner,
                client,
                state.asset.id,
                true,
                sequence.max(adopted_sequence),
                proxy,
            )?;
            if open_guard.load(Ordering::Acquire) != generation {
                return Err("superseded open".into());
            }
            Ok(refreshed)
        },
        move |result| Message::ImportRefreshed(generation, result.map(Box::new)),
    )
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
        async move {
            let (_, sequence) = call(&owner, client, &method, params)?;
            refresh(&owner, client, asset_id, false, sequence, proxy)
        },
        |result| Message::Refreshed(result.map(Box::new)),
    )
}

pub(crate) fn preview_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    entry_id: Option<EntryId>,
    method: &'static str,
    params: Value,
    proxy: Option<ProxyBounds>,
) -> Task<Message> {
    Task::perform(
        async move {
            let (mut result, sequence) = call(&owner, client, method, params)?;
            let session: ClientSession = parse(result["session"].take())?;
            let current = entry_id.is_none();
            let entry = match entry_id.as_ref() {
                Some(id) => id.clone(),
                None => {
                    let (value, _) =
                        call(&owner, client, "asset.state", json!({"asset_id":asset_id}))?;
                    parse::<EditorState>(value)?.current_entry.id
                }
            };
            let expected = PreviewExpectation {
                session_generation: session.preview.generation,
                entry,
                current,
            };
            let job = ready_preview_job(
                &owner,
                proxied(
                    PreviewRequest::new(client, asset_id)
                        .entry(entry_id)
                        .analyse(),
                    proxy,
                ),
                &expected,
            )?;
            Ok(PreviewPayload {
                job,
                session,
                sequence,
            })
        },
        |result| Message::PreviewLoaded(result.map(Box::new)),
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
        |result| Message::RecipeDescribed(result.map(Box::new)),
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
pub(crate) fn crop_preview_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    layer_count: usize,
) -> Task<Message> {
    Task::perform(
        async move {
            let (session, _) = call(&owner, client, "session.state", json!({}))?;
            let session: ClientSession = parse(session)?;
            let (state, _) = call(&owner, client, "asset.state", json!({"asset_id":asset_id}))?;
            let state: EditorState = parse(state)?;
            let expected = PreviewExpectation {
                session_generation: session.preview.generation,
                entry: state.current_entry.id,
                current: true,
            };
            ready_preview_job(
                &owner,
                PreviewRequest::new(client, asset_id).layers(layer_count),
                &expected,
            )
        },
        |result| {
            Message::Crop(crate::app::message::CropMessage::PreviewReady(
                result.map(Box::new),
            ))
        },
    )
}

/// Open this client's one draft of a patch action. The gesture sends nothing else until this
/// answers, so the draft identity every later request needs is known before any of them.
/// `draft.begin` for one generated control's gesture.
///
/// The target is the host's own envelope: for a module action it is the mask the panel's sections
/// are bound to, so the drafted preview shows the masked layer the release will commit rather than
/// the global one; for a `mask.*` command it is the mask and component the gesture edits, which no
/// declared parameter kind could carry. A global gesture sends neither and drafts as it always has.
pub(crate) fn draft_begin_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    action: String,
    target: MaskTarget,
) -> Task<Message> {
    Task::perform(
        async move {
            let (draft, _) = call(
                &owner,
                client,
                "draft.begin",
                draft_begin_params(asset_id, &action, target),
            )?;
            parse::<Draft>(draft)
        },
        |result| Message::SliderDraftBegun(result.map(Box::new)),
    )
}

/// The `draft.begin` request one action and target produce. One spelling, shared by the slider
/// gesture and the mask shape gesture, so the two cannot disagree about where an identity goes.
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
    let job = owner
        .preview_job(proxied(
            PreviewRequest::new(client, asset_id)
                .draft(draft_id)
                .analyse(),
            proxy,
        ))
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

/// Commit the draft once. A real outcome is read back exactly as any other command's is; a no-op
/// outcome created no entry, so nothing is refreshed and the gesture simply ends.
pub(crate) fn draft_commit_task(
    owner: OwnerHandle,
    client: ClientId,
    draft_id: DraftId,
    asset_id: AssetId,
    mutation: Mutation,
    proxy: Option<ProxyBounds>,
) -> Task<Message> {
    Task::perform(
        async move {
            let (committed, sequence) = call(
                &owner,
                client,
                "draft.commit",
                json!({"draft_id":draft_id,"mutation":mutation}),
            )?;
            let result = parse::<MutationResult>(committed)?;
            if result.outcome == MutationOutcome::NoOp {
                return Ok(None);
            }
            refresh(&owner, client, asset_id, false, sequence, proxy).map(Some)
        },
        |result| Message::SliderDraftCommitted(result.map(|refresh| refresh.map(Box::new))),
    )
}

pub(crate) fn draft_cancel_task(
    owner: OwnerHandle,
    client: ClientId,
    draft_id: DraftId,
) -> Task<Message> {
    Task::perform(
        async move { call(&owner, client, "draft.cancel", json!({"draft_id":draft_id})).map(|_| ()) },
        Message::SliderDraftEnded,
    )
}

pub(crate) fn draft_reapply_task(
    owner: OwnerHandle,
    client: ClientId,
    draft_id: DraftId,
) -> Task<Message> {
    Task::perform(
        async move {
            let (draft, _) = call(
                &owner,
                client,
                "draft.reapply",
                json!({"draft_id":draft_id}),
            )?;
            parse::<Draft>(draft)
        },
        |result| Message::SliderDraftReapplied(result.map(Box::new)),
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
        async move {
            let job = owner
                .preview_job(proxied(
                    PreviewRequest::new(client, asset_id)
                        .entry(entry_id)
                        .analyse(),
                    proxy,
                ))
                .map_err(|error| error.to_string())?;
            let (session, sequence) = call(&owner, client, "session.state", json!({}))?;
            Ok(PreviewPayload {
                job,
                session: parse::<ClientSession>(session)?,
                sequence,
            })
        },
        |result| Message::PreviewLoaded(result.map(Box::new)),
    )
}

/// The geometry tail of the displayed stack as one affine, read once when a mask gesture opens.
/// Every later pointer position is mapped from it locally, so a drag costs no host call per move.
pub(crate) fn transform_task(
    owner: OwnerHandle,
    client: ClientId,
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
        Message::MaskTransform,
    )
}

/// `draft.begin` for a `mask.*` gesture: the mask and the component it edits travel in the envelope
/// beside `asset_id`, because no declared parameter kind can carry an identity.
pub(crate) fn mask_draft_begin_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    action: &'static str,
    target: MaskTarget,
) -> Task<Message> {
    Task::perform(
        async move {
            let (draft, _) = call(
                &owner,
                client,
                "draft.begin",
                draft_begin_params(asset_id, action, target),
            )?;
            parse::<Draft>(draft)
        },
        |result| Message::MaskDraftBegun(result.map(Box::new)),
    )
}

/// One `draft.set` of a mask gesture and the one preview job for the geometry it accepted, with the
/// coverage grid the overlay draws filled beside that frame rather than by a second render.
pub(crate) fn mask_draft_set_task(
    owner: OwnerHandle,
    client: ClientId,
    draft_id: DraftId,
    asset_id: AssetId,
    fields: Value,
    proxy: Option<ProxyBounds>,
    overlay: Option<MaskOverlayRequest>,
) -> Task<Message> {
    Task::perform(
        async move {
            let (draft, _) = call(
                &owner,
                client,
                "draft.set",
                json!({"draft_id":draft_id,"fields":fields}),
            )?;
            let draft = parse::<Draft>(draft)?;
            let mut request = PreviewRequest::new(client, asset_id)
                .draft(draft_id)
                .analyse();
            if let Some(overlay) = overlay {
                request = request.mask_overlay(overlay);
            }
            let job = owner
                .preview_job(proxied(request, proxy))
                .map_err(|error| error.to_string())?;
            Ok((draft, job))
        },
        |result| Message::MaskDraftSet(result.map(Box::new)),
    )
}

pub(crate) fn mask_draft_commit_task(
    owner: OwnerHandle,
    client: ClientId,
    draft_id: DraftId,
    asset_id: AssetId,
    mutation: Mutation,
    proxy: Option<ProxyBounds>,
) -> Task<Message> {
    Task::perform(
        async move {
            let (committed, sequence) = call(
                &owner,
                client,
                "draft.commit",
                json!({"draft_id":draft_id,"mutation":mutation}),
            )?;
            // A mask gesture commits through the `mask.*` family, whose answer carries the label
            // and the identities it assigned beside the mutation envelope. Reading it as the bare
            // envelope refuses those fields by name and loses the commit the core already made.
            let result = parse::<MaskCommandResult>(committed)?;
            if result.mutation.outcome == MutationOutcome::NoOp {
                return Ok(None);
            }
            refresh(&owner, client, asset_id, true, sequence, proxy).map(Some)
        },
        |result| Message::MaskDraftCommitted(result.map(|refresh| refresh.map(Box::new))),
    )
}

pub(crate) fn mask_draft_reapply_task(
    owner: OwnerHandle,
    client: ClientId,
    draft_id: DraftId,
) -> Task<Message> {
    Task::perform(
        async move {
            let (draft, _) = call(
                &owner,
                client,
                "draft.reapply",
                json!({"draft_id":draft_id}),
            )?;
            parse::<Draft>(draft)
        },
        |result| Message::MaskDraftReapplied(result.map(Box::new)),
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
            let (result, sequence) = call(&owner, client, method, params)?;
            Ok((parse::<ClientSession>(result)?, sequence))
        },
        Message::SessionUpdated,
    )
}

/// Panel collapse, canvas mode and the thirds overlay are per-client session state the owner holds;
/// the desktop keeps no copy outside the session it adopts back.
pub(crate) fn workspace_task(owner: OwnerHandle, client: ClientId, params: Value) -> Task<Message> {
    Task::perform(
        async move {
            let (result, sequence) = call(&owner, client, "workspace.set", params)?;
            Ok((parse::<ClientSession>(result)?, sequence))
        },
        Message::WorkspaceUpdated,
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
        move |result| Message::PointLocated {
            entry: picked.clone(),
            mode: picked_mode.clone(),
            view: (x, y),
            result,
        },
    )
}

/// One declared module query at a located content pixel, which is what a `sample-apply` canvas
/// mode asks before it commits anything. This is `query.<id>`, the same read-only method an
/// independent client calls: it mutates nothing, writes no history entry and emits no event, and
/// the core answers it from point samples over the compiled evaluation a commit would plan
/// against, so a pick renders no frame. The coordinate parameter names come from the declaration,
/// not from this file.
#[allow(clippy::too_many_arguments)]
pub(crate) fn query_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    entry: EntryId,
    query: String,
    action: String,
    coordinates: (String, String),
    point: (u32, u32),
) -> Task<Message> {
    let answered = entry.clone();
    Task::perform(
        async move {
            let mut params = json!({"asset_id":asset_id,"entry_id":entry});
            let object = params.as_object_mut().expect("the envelope is an object");
            object.insert(coordinates.0, Value::from(point.0));
            object.insert(coordinates.1, Value::from(point.1));
            call(&owner, client, &format!("query.{query}"), params).map(|(value, _)| value)
        },
        move |result| Message::SampleQueried {
            entry: answered.clone(),
            action: action.clone(),
            point,
            result,
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
        move |result| Message::Sampled {
            entry: sampled.clone(),
            result,
        },
    )
}

pub(crate) fn pan_task(owner: OwnerHandle, client: ClientId, x: f32, y: f32) -> Task<Message> {
    Task::perform(
        async move {
            let (result, _) = call(&owner, client, "view.set", json!({"pan_x":x,"pan_y":y}))?;
            parse::<ClientSession>(result)
        },
        Message::PanSynced,
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
            let (_, sequence) = call(&owner, client, method, params)?;
            let (mut listed, seen) =
                call(&owner, client, "version.list", json!({"asset_id":asset_id}))?;
            Ok((
                parse::<Vec<Version>>(listed["versions"].take())?,
                sequence.max(seen),
            ))
        },
        Message::VersionsLoaded,
    )
}

pub(crate) fn sync_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    after: u64,
    proxy: Option<ProxyBounds>,
) -> Task<Message> {
    Task::perform(
        async move { sync_now(&owner, client, asset_id, after, proxy) },
        Message::Synced,
    )
}

/// A method whose event changes the preset library rather than an asset.
fn is_library_event(method: &str) -> bool {
    method.starts_with("preset.")
}

/// One poll: the events since `after`, then only the reads they call for. A poll that saw nothing
/// reads nothing else, and a preset event from another client costs one `preset.list` and no asset
/// refresh or preview.
pub(crate) fn sync_now(
    owner: &OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    after: u64,
    proxy: Option<ProxyBounds>,
) -> Result<SyncResult, String> {
    let (events, mut sequence) = call(owner, client, "events.since", json!({"after":after}))?;
    let events: EventsResult = parse(events)?;
    let library = events.gap
        || events
            .events
            .iter()
            .any(|event| is_library_event(&event.method));
    let asset = events.gap
        || events
            .events
            .iter()
            .any(|event| !is_library_event(&event.method));
    let presets = if library {
        let (presets, seen) = list_presets(owner, client)?;
        sequence = sequence.max(seen);
        Some((presets, seen))
    } else {
        None
    };
    let refresh = if asset {
        let refreshed = refresh(owner, client, asset_id, true, sequence, proxy)?;
        sequence = sequence.max(refreshed.sequence);
        Some(Box::new(refreshed))
    } else {
        None
    };
    Ok(SyncResult {
        sequence,
        refresh,
        presets,
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
    let (result, sequence) = call(owner, client, method, params)?;
    let (presets, seen) = list_presets(owner, client)?;
    Ok(PresetChange {
        result,
        presets,
        sequence: sequence.max(seen),
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
        json!({"content": content, "file_name": file_name, "actor": ACTOR}),
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
    preset_change(owner, client, "preset.delete", json!({"preset_id": id}))
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
        |result| Message::HostAnswered(result.map(Box::new)),
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
            let (page, sequence) = call(
                &owner,
                client,
                "history.list",
                json!({"asset_id":asset_id,"before_sequence":before_sequence,"limit":HISTORY_PAGE_SIZE}),
            )?;
            Ok((parse::<HistoryPage>(page)?, sequence))
        },
        Message::OlderLoaded,
    )
}

pub(crate) fn merge_current_entry(history: &mut HistoryPage, entry: HistoryEntry) {
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

    #[test]
    fn superseded_history_generation_stops_a_waiting_preview() {
        let catalog = std::env::temp_dir().join(format!(
            "lightwell-preview-guard-{}-{}.sqlite",
            std::process::id(),
            REQUEST_NUMBER.fetch_add(1, Ordering::Relaxed)
        ));
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let client = owner.register();
        let asset = AssetId::new();
        let expected = PreviewExpectation {
            session_generation: 0,
            entry: EntryId::new(),
            current: false,
        };
        let _ = call(&owner, client, "preview.return-current", json!({})).unwrap();
        assert!(!preview_still_current(&owner, client, &asset, &expected).unwrap());
        assert_eq!(
            ready_preview_job(
                &owner,
                PreviewRequest::new(client, asset).entry(Some(expected.entry.clone())),
                &expected
            )
            .unwrap_err(),
            "superseded preview"
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn current_entry_merge_is_newest_first_and_bounded() {
        let asset = AssetId::new();
        let mut history = HistoryPage {
            entries: (0..HISTORY_PAGE_SIZE as u64)
                .rev()
                .map(|sequence| entry(&asset, sequence, None))
                .collect(),
            next_before_sequence: None,
        };
        merge_current_entry(&mut history, entry(&asset, HISTORY_PAGE_SIZE as u64, None));
        assert_eq!(history.entries.len(), HISTORY_PAGE_SIZE);
        assert_eq!(history.entries[0].sequence, HISTORY_PAGE_SIZE as u64);
        assert_eq!(history.entries.last().unwrap().sequence, 1);
        assert_eq!(history.next_before_sequence, Some(1));
    }
}
