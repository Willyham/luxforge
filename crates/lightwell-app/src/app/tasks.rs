//! The owner tasks. Every desktop request goes through `call`, which is the same method table the
//! JSON API dispatches; there is no desktop-only mutation path. Each task takes the narrowest
//! completion path the performance rules allow.
use crate::app::message::Message;
use iced::Task;
use lightwell_core::{
    ApiRequest, AssetId, ClientId, ClientSession, ContentPoint, Draft, DraftId, EditorState,
    EntryId, EventsResult, HistoryEntry, HistoryPage, HistorySelection, Lineage, ModuleDescriptor,
    Mutation, MutationOutcome, MutationResult, OwnerHandle, PreviewJob, PreviewRequest,
    RecipeDescription, Version,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
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
    pub(crate) started: Instant,
}

#[derive(Clone, Debug)]
pub(crate) enum SyncResult {
    Unchanged { sequence: u64 },
    Changed(Box<Refresh>),
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

/// Read authoritative state back after a change or an external event.
pub(crate) fn refresh(
    owner: &OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    with_history: bool,
    mut sequence: u64,
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
    // The Original entry is sequence 0, so one bounded page before sequence 1 finds it.
    let original: HistoryPage = parse(fetch(
        "history.list",
        json!({"asset_id":asset_id,"before_sequence":1,"limit":1}),
    )?)?;
    let job = owner
        .preview_job(PreviewRequest::new(client, asset_id).entry(selected))
        .map_err(|error| error.to_string())?;
    Ok(Refresh {
        state,
        history,
        versions,
        lineage,
        recipe,
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

pub(crate) fn import_task(owner: OwnerHandle, client: ClientId, path: PathBuf) -> Task<Message> {
    Task::perform(
        async move {
            let (result, sequence) = call(&owner, client, "catalog.import", json!({"path":path}))?;
            let state: EditorState = parse(result)?;
            let _ = call(&owner, client, "preview.return-current", json!({}))?;
            refresh(&owner, client, state.asset.id, true, sequence)
        },
        |result| Message::Refreshed(result.map(Box::new)),
    )
}

pub(crate) fn state_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    method: String,
    params: Value,
) -> Task<Message> {
    Task::perform(
        async move {
            let (_, sequence) = call(&owner, client, &method, params)?;
            refresh(&owner, client, asset_id, false, sequence)
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
) -> Task<Message> {
    Task::perform(
        async move {
            let (mut result, sequence) = call(&owner, client, method, params)?;
            let session: ClientSession = parse(result["session"].take())?;
            let job = owner
                .preview_job(PreviewRequest::new(client, asset_id).entry(entry_id))
                .map_err(|error| error.to_string())?;
            Ok(PreviewPayload {
                job,
                session,
                sequence,
            })
        },
        |result| Message::PreviewLoaded(result.map(Box::new)),
    )
}

/// The displayed entry's layers, read after a history selection changed which entry is shown. It
/// reads payloads only: no decode, no render, no source access.
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
            parse::<RecipeDescription>(described)
        },
        |result| Message::RecipeDescribed(result.map(Box::new)),
    )
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
            owner
                .preview_job(PreviewRequest::new(client, asset_id).layers(layer_count))
                .map_err(|error| error.to_string())
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
pub(crate) fn draft_begin_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    action: String,
) -> Task<Message> {
    Task::perform(
        async move {
            let (draft, _) = call(
                &owner,
                client,
                "draft.begin",
                json!({"asset_id":asset_id,"action":action}),
            )?;
            parse::<Draft>(draft)
        },
        |result| Message::SliderDraftBegun(result.map(Box::new)),
    )
}

/// One `draft.set` and the one preview job for the settings it accepted, as a single round trip.
/// The gesture's bound is one of these per tick, so pairing them here is what keeps a preview from
/// being requested for settings the core never accepted.
pub(crate) fn draft_set_task(
    owner: OwnerHandle,
    client: ClientId,
    draft_id: DraftId,
    asset_id: AssetId,
    fields: Value,
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
            let job = owner
                .preview_job(PreviewRequest::new(client, asset_id).draft(draft_id))
                .map_err(|error| error.to_string())?;
            Ok((draft, job))
        },
        |result| Message::SliderDraftSet(result.map(Box::new)),
    )
}

/// Commit the draft once. A real outcome is read back exactly as any other command's is; a no-op
/// outcome created no entry, so nothing is refreshed and the gesture simply ends.
pub(crate) fn draft_commit_task(
    owner: OwnerHandle,
    client: ClientId,
    draft_id: DraftId,
    asset_id: AssetId,
    mutation: Mutation,
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
            refresh(&owner, client, asset_id, false, sequence).map(Some)
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
/// request and no history refresh.
pub(crate) fn current_preview_task(
    owner: OwnerHandle,
    client: ClientId,
    asset_id: AssetId,
    entry_id: Option<EntryId>,
) -> Task<Message> {
    Task::perform(
        async move {
            let job = owner
                .preview_job(PreviewRequest::new(client, asset_id).entry(entry_id))
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
    x: u32,
    y: u32,
) -> Task<Message> {
    let picked = entry.clone();
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
            view: (x, y),
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
) -> Task<Message> {
    Task::perform(
        async move {
            let (events, sequence) = call(&owner, client, "events.since", json!({"after":after}))?;
            let events: EventsResult = parse(events)?;
            if events.events.is_empty() && !events.gap {
                Ok(SyncResult::Unchanged { sequence })
            } else {
                refresh(&owner, client, asset_id, true, sequence)
                    .map(Box::new)
                    .map(SyncResult::Changed)
            }
        },
        Message::Synced,
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
