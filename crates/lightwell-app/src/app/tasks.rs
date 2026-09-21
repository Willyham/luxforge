//! The owner tasks. Every desktop request goes through `call`, which is the same method table the
//! JSON API dispatches; there is no desktop-only mutation path. Each task takes the narrowest
//! completion path the performance rules allow.
use crate::app::message::Message;
use iced::Task;
use lightwell_core::{
    ApiRequest, AssetId, ClientId, ClientSession, ContentPoint, EditorState, EntryId, EventsResult,
    HistoryEntry, HistoryPage, HistorySelection, Lineage, ModuleDescriptor, Mutation, OwnerHandle,
    PreviewJob, RecipeDescription, Version,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
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
    client: ClientId,
    asset_id: AssetId,
    entry_id: Option<EntryId>,
    layer_count: Option<usize>,
    expected: &PreviewExpectation,
) -> Result<PreviewJob, String> {
    loop {
        if !preview_still_current(owner, client, &asset_id, expected)? {
            return Err("superseded preview".into());
        }
        match owner.preview_job(client, asset_id.clone(), entry_id.clone(), layer_count) {
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
    let expected = PreviewExpectation {
        session_generation: session.preview.generation,
        entry: selected
            .clone()
            .unwrap_or_else(|| state.current_entry.id.clone()),
        current: selected.is_none(),
    };
    let job = ready_preview_job(owner, client, asset_id, selected, None, &expected)?;
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

pub(crate) fn import_task(
    owner: OwnerHandle,
    client: ClientId,
    path: PathBuf,
    generation: u64,
    open_guard: Arc<AtomicU64>,
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
            let job = ready_preview_job(&owner, client, asset_id, entry_id, None, &expected)?;
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
            let (session, _) = call(&owner, client, "session.state", json!({}))?;
            let session: ClientSession = parse(session)?;
            let (state, _) = call(&owner, client, "asset.state", json!({"asset_id":asset_id}))?;
            let state: EditorState = parse(state)?;
            let expected = PreviewExpectation {
                session_generation: session.preview.generation,
                entry: state.current_entry.id,
                current: true,
            };
            ready_preview_job(&owner, client, asset_id, None, Some(layer_count), &expected)
        },
        |result| {
            Message::Crop(crate::app::message::CropMessage::PreviewReady(
                result.map(Box::new),
            ))
        },
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
                client,
                asset,
                Some(expected.entry.clone()),
                None,
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
