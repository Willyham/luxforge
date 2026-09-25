//! The JSON owner API: protocol types, the single catalog owner, the method table and the
//! loopback transport. Every client, including the desktop, drives the same methods.
mod methods;
pub(crate) mod params;
#[cfg(test)]
pub(crate) use methods::host_envelope;
mod owner;
mod transport;

pub use methods::schemas;
pub use owner::{ClientId, OwnerHandle, PreviewRequest};

pub use transport::{LocalServer, LocalSessionInfo, serve_json_lines, serve_json_lines_with};

use crate::{Draft, DraftId, Error, ErrorKind, PreviewSession};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL: &str = "luxforge-jsonl-1";
/// The diagnostic components board has these zero-based pages. It is per-client workspace state.
pub const COMPONENT_GALLERY_PAGE_COUNT: usize = 10;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    /// Structured context for the failure, e.g. `{consent: …}` or `{requirements: […]}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
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
                job_id: error.preparation_job().map(ToString::to_string),
                message: error.detail,
                data: error.data.map(|data| *data),
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

/// What the canvas draws of the selected mask, per the [masking
/// design](../../../docs/design/masking.md)'s overlay.
///
/// Per-client view state exactly as the clipping flags are: it changes no recipe, no histogram
/// population and no export, and it commits nothing. What it selects is how the coverage grid the
/// preview worker returns beside the frame is painted, never whether one is correct.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaskOverlayMode {
    /// The photograph alone.
    #[default]
    Off,
    /// The selected mask as a tint over the photograph, at the coverage of each cell.
    Tint,
    /// The mask alone on black: coverage as a greyscale, with no photograph behind it.
    MaskOnBlack,
    /// The photograph seen through the mask, on black.
    ImageOnBlack,
}

impl MaskOverlayMode {
    /// Every mode, in the order the design lists them. The accepted vocabulary is read from here,
    /// so a mode and its spelling cannot drift apart.
    pub const ALL: [Self; 4] = [Self::Off, Self::Tint, Self::MaskOnBlack, Self::ImageOnBlack];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Tint => "tint",
            Self::MaskOnBlack => "mask-on-black",
            Self::ImageOnBlack => "image-on-black",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|mode| mode.as_str() == value)
    }
}

/// The tint a mask overlay is drawn in.
///
/// A name, not a colour: the value each name resolves to is a design token in
/// `crates/luxforge-ui/src/theme.rs`, so the core never holds a colour and a client cannot send
/// one. Every name here is drawn in a tint a person can tell apart from the clipping indicators,
/// which already own red and blue on this canvas.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MaskOverlayColour {
    #[default]
    Green,
    White,
}

impl MaskOverlayColour {
    pub const ALL: [Self; 2] = [Self::Green, Self::White];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::White => "white",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|colour| colour.as_str() == value)
    }
}

/// Per-client workspace state: which panels are open, which canvas mode is active, whether the
/// thirds overlay is on, which clipping overlays are shown, what the canvas draws of the selected
/// mask and which diagnostic gallery page is open. It is a client preference the owner holds, never
/// authoritative edit state: an overlay or gallery page never alters the raster, saved recipe,
/// histogram population or a future export.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceState {
    pub state_panel: bool,
    pub tools_panel: bool,
    /// `pointer`, or the id of an available module that declares a canvas interaction.
    pub mode: String,
    pub thirds: bool,
    /// Show the shadow (any channel at code 0) clipping overlay.
    #[serde(default)]
    pub clip_shadows: bool,
    /// Show the highlight (any channel at code 255) clipping overlay.
    #[serde(default)]
    pub clip_highlights: bool,
    /// What the canvas draws of the selected mask.
    #[serde(default)]
    pub mask_overlay: MaskOverlayMode,
    /// The tint [`MaskOverlayMode::Tint`] is drawn in.
    #[serde(default)]
    pub mask_overlay_colour: MaskOverlayColour,
    /// Zero-based diagnostic components page, or `None` for the editor workspace.
    pub component_gallery: Option<usize>,
}

/// The pointer mode: the canvas shows the photograph and nothing else.
pub const POINTER_MODE: &str = "pointer";

/// The mask mode: the canvas draws the selected mask's handles and the tools panel shows the Masks
/// panel in place of the module sections.
///
/// It is a host mode and not a module's, because a mask is a host object in the recipe rather than a
/// tool module ([masking design](../../../docs/design/masking.md)). It is therefore always offered,
/// exactly as the pointer is, and needs no module to declare a canvas interaction for it.
pub const MASK_MODE: &str = "mask";

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            state_panel: true,
            tools_panel: true,
            mode: POINTER_MODE.into(),
            thirds: false,
            clip_shadows: false,
            clip_highlights: false,
            mask_overlay: MaskOverlayMode::Off,
            mask_overlay_colour: MaskOverlayColour::Green,
            component_gallery: None,
        }
    }
}

/// What a client may do beyond editing, fixed when it registers and forgotten when it disconnects.
/// Only a client with permission authority may grant a module permission: the desktop's own client,
/// which grants only after the person presses Allow, and `luxforge-json --permission-authority`,
/// an explicit local setup step. Loopback live-session clients and plain `luxforge-json` edit only;
/// anyone may deny or revoke, since both reduce privilege.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClientAuthority {
    #[default]
    Edit,
    Permissions,
}

impl ClientAuthority {
    /// The label a grant or denial records as its actor.
    pub fn label(self) -> &'static str {
        match self {
            Self::Edit => "edit",
            Self::Permissions => "permissions",
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
    /// The authority this client registered with. The owner sets it before the client's first call
    /// and no method changes it.
    #[serde(default)]
    pub authority: ClientAuthority,
}

impl ClientSession {
    pub(super) fn touch(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    /// The draft this client holds under that identity. Another client's draft, or one that has
    /// already ended, is simply not this session's. Every method and preview job that names a draft
    /// resolves it here.
    pub(super) fn held_draft(&self, draft_id: &DraftId) -> Result<&Draft, Error> {
        self.draft
            .as_ref()
            .filter(|draft| &draft.draft_id == draft_id)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Validation,
                    format!("unknown draft {draft_id} for this client"),
                )
            })
    }
}
