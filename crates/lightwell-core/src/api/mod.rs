//! The JSON owner API: protocol types, the single catalog owner, the method table and the
//! loopback transport. Every client, including the desktop, drives the same methods.
mod methods;
mod owner;
mod transport;

pub use methods::schemas;
pub use owner::{ClientId, OwnerHandle, PreviewRequest};
pub use transport::{LocalServer, LocalSessionInfo, serve_json_lines};

use crate::{Draft, Error, ErrorKind, PreviewSession};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL: &str = "lightwell-jsonl-1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiRequest {
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiFailure {
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiResponse {
    pub id: String,
    pub sequence: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiFailure>,
}

impl ApiResponse {
    pub(super) fn success(id: String, sequence: u64, result: impl Serialize) -> Self {
        match serde_json::to_value(result) {
            Ok(result) => Self {
                id,
                sequence,
                result: Some(result),
                error: None,
            },
            Err(error) => Self::failure(
                id,
                sequence,
                Error::new(ErrorKind::Internal, error.to_string()),
            ),
        }
    }
    pub(super) fn failure(id: String, sequence: u64, error: Error) -> Self {
        Self {
            id,
            sequence,
            result: None,
            error: Some(ApiFailure {
                code: error.kind.code().into(),
                message: error.detail,
            }),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiEvent {
    pub sequence: u64,
    pub method: String,
    pub request_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsResult {
    pub events: Vec<ApiEvent>,
    pub current_sequence: u64,
    pub gap: bool,
}

/// Per-client workspace state: which panels are open, which canvas mode is active and whether the
/// thirds overlay is on. It is a client preference the owner holds, never authoritative edit state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceState {
    pub state_panel: bool,
    pub tools_panel: bool,
    /// `pointer`, or the id of an available module that declares a canvas interaction.
    pub mode: String,
    pub thirds: bool,
}

/// The pointer mode: the canvas shows the photograph and nothing else.
pub const POINTER_MODE: &str = "pointer";

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            state_panel: true,
            tools_panel: true,
            mode: POINTER_MODE.into(),
            thirds: false,
        }
    }
}

/// Per-client session state held by the owner. `revision` increases on every session change so a
/// client applying responses out of order can keep the newest one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClientSession {
    pub preview: PreviewSession,
    #[serde(default)]
    pub workspace: WorkspaceState,
    /// The one draft this client holds, if any. Session state: it emits no event, appears in no
    /// history and never outlives the session.
    #[serde(default)]
    pub draft: Option<Draft>,
    #[serde(default)]
    pub revision: u64,
}

impl ClientSession {
    pub(super) fn touch(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }
}
