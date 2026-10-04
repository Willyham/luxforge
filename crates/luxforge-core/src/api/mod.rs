//! The JSON owner API: protocol types, the single catalog owner, the method table and the
//! loopback transport. Every client, including the desktop, drives the same methods.
mod methods;
pub(crate) mod params;
#[cfg(test)]
pub(crate) use methods::host_envelope;
mod owner;
mod transport;

pub use methods::schemas;
pub(crate) use owner::SourceFlightKey;
pub use owner::{ClientId, EventWake, OwnerHandle, PreviewRequest};

pub use transport::{LocalServer, serve_json_lines_with};

use crate::{AssetId, Draft, DraftId, Error, PreviewSession};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub(crate) const PROTOCOL: &str = "luxforge-jsonl-1";

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
    /// An answer that is already a JSON value, moved in as it is. Every method's answer reaches
    /// the owner as a `Value`, so converting it again would rebuild the whole tree (the largest
    /// are `asset.state` and a brush `draft.set`) on the owner thread for no change.
    pub(super) fn value(id: String, sequence: u64, result: Value) -> Self {
        Self {
            id,
            sequence,
            result: Some(result),
            error: None,
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

/// One change in the owner's event log: the request it was made under and, when the change has
/// one, the asset it changed and the revision it left that asset at. A version names its asset but
/// no revision, since naming an entry moves none; a change to the preset library, a module or the
/// artifact store names no asset.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiEvent {
    pub sequence: u64,
    pub method: String,
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<AssetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
}

/// What a change is announced as: the method and request identity a client watching
/// `events.since` sees, and the asset and revision it changed when it has them. A job carries the
/// origin of the request that started it, and a change it causes later is announced under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Origin {
    pub method: String,
    pub request_id: String,
    pub asset_id: Option<AssetId>,
    pub revision: Option<u64>,
}

impl Origin {
    pub(crate) fn new(method: &str, request_id: &str) -> Self {
        Self {
            method: method.to_owned(),
            request_id: request_id.to_owned(),
            asset_id: None,
            revision: None,
        }
    }

    /// The same request, naming the asset it changed and, when that moved it, the revision it left.
    pub(crate) fn changed(mut self, asset_id: AssetId, revision: Option<u64>) -> Self {
        self.asset_id = Some(asset_id);
        self.revision = revision;
        self
    }
}

/// Announce `origin` once, however many changes a request made.
pub(crate) fn announce_once(announce: &mut Vec<Origin>, origin: &Origin) {
    if !announce.contains(origin) {
        announce.push(origin.clone());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventsResult {
    pub events: Vec<ApiEvent>,
    pub current_sequence: u64,
    pub gap: bool,
}

/// What the canvas draws of the selected mask, per the [masking
/// design](../../../../docs/design/masking.md)'s overlay.
///
/// Per-client view state exactly as the clipping flags are: it changes no recipe, no histogram
/// population and no export, and it commits nothing. What it selects is how a mask's coverage grid
/// ([`crate::Evaluation::mask_overlay_coverage`]) is painted, never whether one is correct.
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

    pub(crate) fn parse(value: &str) -> Option<Self> {
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

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|colour| colour.as_str() == value)
    }
}

/// Per-client workspace state: which panels are open, which canvas mode is active, whether the
/// thirds overlay is on, which clipping overlays are shown, what the canvas draws of the selected
/// mask and whether gestures preview on the GPU. It is a client preference the owner holds, never
/// authoritative edit state: an overlay never alters the raster, saved recipe, histogram population
/// or a future export, and the GPU preview changes only what is drawn while a gesture moves. The
/// desktop's developer components gallery is not here: which page it shows is that desktop's own
/// view state.
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
    /// Draw this client's gestures through the GPU preview stage where it can, on by default.
    /// Off, every gesture previews on the CPU. Either way the settled frame, the histogram,
    /// samples, analysis, export and every API answer are the CPU's.
    #[serde(default = "WorkspaceState::gpu_preview_default")]
    pub gpu_preview: bool,
}

/// The pointer mode: the canvas shows the photograph and nothing else.
pub const POINTER_MODE: &str = "pointer";

/// The mask mode: the canvas draws the selected mask's handles and the tools panel shows the Masks
/// panel in place of the module sections.
///
/// It is a host mode and not a module's, because a mask is a host object in the recipe rather than a
/// tool module ([masking design](../../../../docs/design/masking.md)). It is therefore always offered,
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
            gpu_preview: Self::gpu_preview_default(),
        }
    }
}

impl WorkspaceState {
    /// The GPU preview's recorded default: on.
    const fn gpu_preview_default() -> bool {
        true
    }
}

/// Which renderer draws the desktop's picture: the GPU, or the CPU reference renderer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RendererRecord {
    Gpu,
    #[default]
    Reference,
}

/// Why the reference renderer draws the desktop's picture rather than the GPU: the desktop's photo
/// surface's own reason, as its frames name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RendererReason {
    /// The photo surface has not checked its GPU stage yet: no photograph has been drawn.
    SurfacePending,
    /// The GPU stage cannot run on this graphics device, or the launch refused it
    /// (`--no-gpu-render`), which the stage's capability check answers the same way.
    NoAdapter,
    /// The graphics device was lost; nothing waits for a recovery.
    DeviceLost,
}

impl RendererReason {
    /// Every reason, in the order the session's description lists them.
    pub const ALL: [Self; 3] = [Self::SurfacePending, Self::NoAdapter, Self::DeviceLost];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::SurfacePending => "surface-pending",
            Self::NoAdapter => "no-adapter",
            Self::DeviceLost => "device-lost",
        }
    }
}

/// Which renderer draws the desktop's picture on this machine, and why the reference does, as
/// `{record, reason}` (`docs/design/gpu-first.md`): `{record: "gpu", reason: null}` while the
/// desktop's photo surface can draw on its GPU, and `{record: "reference", reason}` while it
/// cannot. An owner that draws nothing, `luxforge-json`'s, always reports the reference with no
/// reason: it has no GPU stage to fall back from.
///
/// The process hosting the owner reports it ([`OwnerHandle::report_renderer`], a desktop-internal
/// path), every client's session carries the owner's one value, and no method sets it, so no
/// client can claim a renderer the desktop does not have. A GPU record never has a reason; a
/// session read with one is refused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RendererFields")]
pub struct Renderer {
    record: RendererRecord,
    reason: Option<RendererReason>,
}

/// [`Renderer`]'s fields as they are read, before the GPU record's missing reason is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RendererFields {
    record: RendererRecord,
    reason: Option<RendererReason>,
}

impl TryFrom<RendererFields> for Renderer {
    type Error = &'static str;

    fn try_from(fields: RendererFields) -> Result<Self, Self::Error> {
        match fields {
            RendererFields {
                record: RendererRecord::Gpu,
                reason: Some(_),
            } => Err("a GPU renderer has no reason"),
            RendererFields { record, reason } => Ok(Self { record, reason }),
        }
    }
}

impl Renderer {
    /// The desktop's photo surface draws on its GPU.
    pub const fn gpu() -> Self {
        Self {
            record: RendererRecord::Gpu,
            reason: None,
        }
    }

    /// The reference renderer draws the desktop's picture, for `reason`.
    pub const fn reference(reason: RendererReason) -> Self {
        Self {
            record: RendererRecord::Reference,
            reason: Some(reason),
        }
    }

    /// An owner that draws nothing: the reference renderer is its only renderer.
    pub const fn headless() -> Self {
        Self {
            record: RendererRecord::Reference,
            reason: None,
        }
    }

    pub fn record(self) -> RendererRecord {
        self.record
    }

    pub fn reason(self) -> Option<RendererReason> {
        self.reason
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
    #[serde(skip)]
    pub pixel_memo: crate::editor::pixels::PixelMemo,
    #[serde(default)]
    pub revision: u64,
    /// The authority this client registered with. The owner sets it before the client's first call
    /// and no method changes it.
    #[serde(default)]
    pub authority: ClientAuthority,
    /// Which renderer draws the desktop's picture on this machine: the owner's one value, the same
    /// in every client's session, which the owner sets before each call and no method changes.
    #[serde(default)]
    pub renderer: Renderer,
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
            .ok_or_else(|| Error::validation(format!("unknown draft {draft_id} for this client")))
    }
}
