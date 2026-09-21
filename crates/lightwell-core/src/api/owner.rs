//! One thread owns the catalog and every client session; all clients call it in turn.
use super::{ApiEvent, ApiRequest, ApiResponse, ClientSession, EventsResult, methods};
use crate::{AssetId, EditorService, EntryId, Error, ErrorKind, ModuleRegistry, PreviewJob};
use serde::Deserialize;
use std::{
    collections::{HashMap, VecDeque},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::JoinHandle,
};

const EVENT_CAPACITY: usize = 256;

/// Identifies one connected client; the owner keeps that client's session until it disconnects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(u64);

struct OwnerCall {
    client: ClientId,
    request: ApiRequest,
    response: SyncSender<ApiResponse>,
}

enum OwnerMessage {
    Call(OwnerCall),
    Preview {
        asset_id: AssetId,
        entry_id: Option<EntryId>,
        /// `Some(n)` renders the entry's first `n` layers only.
        layer_count: Option<usize>,
        response: SyncSender<Result<PreviewJob, Error>>,
    },
    Disconnect(ClientId),
    Stop,
}

#[derive(Clone)]
pub struct OwnerHandle {
    sender: SyncSender<OwnerMessage>,
    next_client: Arc<AtomicU64>,
}

impl OwnerHandle {
    pub fn start(catalog: &Path) -> Result<(Self, JoinHandle<()>), Error> {
        Self::start_with(catalog, Arc::new(ModuleRegistry::builtin()))
    }

    /// Own a catalog served by a specific set of providers, which is how a client registers a
    /// built-in wrapped as unavailable. Registration happens before any catalog work.
    pub fn start_with(
        catalog: &Path,
        registry: Arc<ModuleRegistry>,
    ) -> Result<(Self, JoinHandle<()>), Error> {
        let service = EditorService::open_with(catalog, registry)?;
        let (sender, receiver) = sync_channel(64);
        let join = std::thread::spawn(move || owner_loop(service, receiver));
        Ok((
            Self {
                sender,
                next_client: Arc::new(AtomicU64::new(1)),
            },
            join,
        ))
    }

    /// Allocate a client identity; its session starts as default on first use.
    pub fn register(&self) -> ClientId {
        ClientId(self.next_client.fetch_add(1, Ordering::Relaxed))
    }

    /// Forget a client's session. Its committed edits and jobs are unaffected.
    pub fn disconnect(&self, client: ClientId) {
        let _ = self.sender.send(OwnerMessage::Disconnect(client));
    }

    pub fn call(&self, client: ClientId, request: ApiRequest) -> Result<ApiResponse, Error> {
        let (sender, receiver) = sync_channel(1);
        self.sender
            .send(OwnerMessage::Call(OwnerCall {
                client,
                request,
                response: sender,
            }))
            .map_err(|_| Error::new(ErrorKind::Protocol, "catalog owner is unavailable"))?;
        receiver.recv().map_err(|_| {
            Error::new(
                ErrorKind::Protocol,
                "catalog owner stopped before responding",
            )
        })
    }

    pub fn stop(&self) {
        let _ = self.sender.send(OwnerMessage::Stop);
    }

    /// A preview job from the catalog owner. `layer_count` truncates the rendered stack to its
    /// first `n` layers; this is a desktop-internal path, not a JSON method.
    pub fn preview_job(
        &self,
        asset_id: AssetId,
        entry_id: Option<EntryId>,
        layer_count: Option<usize>,
    ) -> Result<PreviewJob, Error> {
        let (response, receiver) = sync_channel(1);
        self.sender
            .send(OwnerMessage::Preview {
                asset_id,
                entry_id,
                layer_count,
                response,
            })
            .map_err(|_| Error::new(ErrorKind::Protocol, "catalog owner is unavailable"))?;
        receiver
            .recv()
            .map_err(|_| Error::new(ErrorKind::Protocol, "catalog owner stopped before preview"))?
    }
}

fn owner_loop(mut service: EditorService, receiver: Receiver<OwnerMessage>) {
    let mut sequence = 0u64;
    let mut events = VecDeque::with_capacity(EVENT_CAPACITY);
    let mut sessions: HashMap<ClientId, ClientSession> = HashMap::new();
    while let Ok(message) = receiver.recv() {
        match message {
            OwnerMessage::Stop => break,
            OwnerMessage::Disconnect(client) => {
                sessions.remove(&client);
            }
            OwnerMessage::Preview {
                asset_id,
                entry_id,
                layer_count,
                response,
            } => {
                let _ =
                    response.send(service.preview_job(&asset_id, entry_id.as_ref(), layer_count));
            }
            OwnerMessage::Call(call) => {
                let session = sessions.entry(call.client).or_default();
                // Discovery and dispatch resolve through the same registry-aware lookup.
                let method = methods::find(&service, &call.request.method);
                let response = match method {
                    Some(method) if method.owner_answered() => {
                        event_response(&call.request, sequence, &events)
                    }
                    Some(method) => {
                        let response =
                            methods::dispatch(&mut service, session, &call.request, sequence);
                        if response.error.is_none()
                            && methods::mutates(&method, response.result.as_ref())
                        {
                            sequence = sequence.saturating_add(1);
                            if events.len() == EVENT_CAPACITY {
                                events.pop_front();
                            }
                            events.push_back(ApiEvent {
                                sequence,
                                method: call.request.method.clone(),
                                request_id: call.request.id.clone(),
                            });
                            ApiResponse {
                                sequence,
                                ..response
                            }
                        } else {
                            response
                        }
                    }
                    None => methods::dispatch(&mut service, session, &call.request, sequence),
                };
                let _ = call.response.send(response);
            }
        }
    }
}

fn event_response(request: &ApiRequest, sequence: u64, events: &VecDeque<ApiEvent>) -> ApiResponse {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Params {
        after: u64,
    }
    match methods::params::<Params>(&request.params) {
        Ok(params) => {
            let oldest = events
                .front()
                .map(|event| event.sequence)
                .unwrap_or(sequence.saturating_add(1));
            let gap = params.after.saturating_add(1) < oldest;
            let selected = events
                .iter()
                .filter(|event| event.sequence > params.after)
                .cloned()
                .collect();
            ApiResponse::success(
                request.id.clone(),
                sequence,
                EventsResult {
                    events: selected,
                    current_sequence: sequence,
                    gap,
                },
            )
        }
        Err(error) => ApiResponse::failure(request.id.clone(), sequence, error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("lightwell-owner-{}-{name}", std::process::id()))
    }
    fn fixture() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0/orientation-1.jpg")
    }

    #[test]
    fn an_owner_started_with_a_registry_serves_exactly_those_providers() {
        let catalog = temp("registry.sqlite");
        let _ = std::fs::remove_file(&catalog);
        let (owner, join) =
            OwnerHandle::start_with(&catalog, Arc::new(crate::ModuleRegistry::new())).unwrap();
        let client = owner.register();
        let response = owner
            .call(
                client,
                ApiRequest {
                    id: "modules".into(),
                    method: "module.list".into(),
                    params: json!({}),
                    token: None,
                },
            )
            .unwrap();
        assert_eq!(
            response.result.unwrap()["modules"],
            json!([]),
            "the owner serves the registry it was given, not the built-ins"
        );
        let missing = owner
            .call(
                client,
                ApiRequest {
                    id: "pixel".into(),
                    method: "edit.set-pixel".into(),
                    params: json!({}),
                    token: None,
                },
            )
            .unwrap();
        assert_eq!(
            missing.error.expect("no provider, no method").message,
            "unknown method edit.set-pixel"
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }

    #[test]
    fn historical_preview_stays_selected_during_another_clients_commit() {
        let catalog = temp("preview-live.sqlite");
        let (owner, join) = OwnerHandle::start(&catalog).unwrap();
        let viewer = owner.register();
        let agent = owner.register();
        assert_ne!(viewer, agent);
        let call = |client: ClientId, id: &str, method: &str, params: Value| {
            let response = owner
                .call(
                    client,
                    ApiRequest {
                        id: id.into(),
                        method: method.into(),
                        params,
                        token: None,
                    },
                )
                .unwrap();
            assert!(response.error.is_none(), "{id}: {:?}", response.error);
            response.result.unwrap()
        };
        let imported = call(
            viewer,
            "import",
            "catalog.import",
            json!({"path":fixture()}),
        );
        let asset = imported["asset"]["id"].clone();
        let original = imported["current_entry"]["id"].clone();
        let baseline = call(
            viewer,
            "baseline",
            "render.sample",
            json!({"asset_id":asset,"x":0,"y":0}),
        )["rgba"]
            .clone();
        let edited = call(
            viewer,
            "a",
            "edit.set-pixel",
            json!({"asset_id":asset,"mutation":{"expected_revision":0,"request_id":"a","actor":"viewer"},"x":0,"y":0,"rgb":[1,2,3]}),
        );
        // Selecting the current entry is the live state, not a historical preview.
        let latest = call(
            viewer,
            "latest",
            "preview.select",
            json!({"asset_id":asset,"entry_id":edited["current_entry_id"]}),
        );
        assert_eq!(latest["session"]["preview"]["selection"], json!("current"));
        assert!(latest["generation"].is_u64());
        let selected = call(
            viewer,
            "preview",
            "preview.select",
            json!({"asset_id":asset,"entry_id":original}),
        );
        assert_eq!(selected["session"]["revision"], json!(2));
        assert_eq!(
            selected["session"]["preview"]["selection"],
            json!({"entry": original})
        );
        // The other client's session is independent and unaffected.
        assert_eq!(
            call(agent, "agent-session", "session.state", json!({}))["revision"],
            json!(0)
        );
        call(
            agent,
            "b",
            "edit.set-pixel",
            json!({"asset_id":asset,"mutation":{"expected_revision":1,"request_id":"b","actor":"agent"},"x":0,"y":0,"rgb":[9,8,7]}),
        );
        let preview = call(
            viewer,
            "sample-old",
            "render.sample",
            json!({"asset_id":asset,"x":0,"y":0}),
        );
        assert_eq!(preview["rgba"], baseline);
        let returned = call(viewer, "current", "preview.return-current", json!({}));
        assert_eq!(returned["session"]["revision"], json!(3));
        let current = call(
            viewer,
            "sample-current",
            "render.sample",
            json!({"asset_id":asset,"x":0,"y":0}),
        );
        assert_eq!(current["rgba"], json!([9, 8, 7, 255]));
        // Disconnecting forgets the session; a fresh registration starts from default.
        call(
            viewer,
            "zoom",
            "view.set",
            json!({"zoom":{"mode":"percent","value":200.0}}),
        );
        owner.disconnect(viewer);
        let fresh = owner.register();
        assert_eq!(
            call(fresh, "fresh", "session.state", json!({}))["preview"]["view"]["zoom"]["mode"],
            json!("fit")
        );
        owner.stop();
        join.join().unwrap();
        std::fs::remove_file(catalog).unwrap();
    }
}
