//! The JSON owner API: protocol types, the single catalog owner, the method table and the
//! loopback transport. Every client, including the desktop, drives the same methods.
mod methods;
pub(crate) mod params;
#[cfg(test)]
pub(crate) use methods::host_envelope;
mod owner;
mod transport;

pub use methods::schemas;
pub use owner::{ClientId, EventWake, OwnerHandle, PIXEL_READ_REQUIRED, PreviewRequest};
pub(crate) use owner::{OWNER_THREAD, SourceFlightKey};

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
/// thirds and information overlays are on, which clipping overlays are shown, what the canvas draws
/// of the selected mask and whether gestures preview on the GPU. It is a client preference the owner holds, never
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
    /// Show the image-information overlay, off by default. Changes no recipe or preview generation.
    pub information: bool,
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
            information: false,
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

/// Why the reference renderer rendered rather than the GPU, as its stable kebab-case code.
///
/// The session's three reasons say why the reference draws the desktop's picture, as its photo
/// surface names them ([`Self::ALL`]). An export's result names the renderer that rendered its file
/// in the same shape, with those reasons and the export's own ([`Self::EXPORT`], and the GPU plan's
/// code): the tile service's reason the GPU could not render it (`docs/design/export.md`).
///
/// It is read back only as a session carries it, with the session's reasons: an export's reason is
/// written in its result, which no client reads back into a session, and a session read with one
/// is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RendererReason {
    /// The photo surface has not checked its GPU stage yet: no photograph has been drawn. For an
    /// export, the desktop has not yet named the adapter its window draws with to its tile worker,
    /// which it does once the surface has checked its stage.
    SurfacePending,
    /// The GPU stage cannot run on this graphics device, or the launch refused it
    /// (`--no-gpu-render`), which the stage's capability check answers the same way. For an
    /// export, the tile worker found no adapter or device it can render on.
    NoAdapter,
    /// The graphics device was lost; nothing waits for a recovery.
    DeviceLost,
    /// An export asked for the reference renderer (`reference: true`).
    Requested,
    /// The launch refused the GPU (`--no-gpu-render`), so the tile worker renders nothing.
    Refused,
    /// The tile worker's adapter is not the one the window draws with, so it renders nothing.
    AdapterMismatch,
    /// The export's tiles would hold more than the tile worker's budget (`tiles-budget`).
    Budget,
    /// The GPU cannot draw the stack's plan, for the code named: the plan's own fallback, such as
    /// `pixel-stage` for a layer no GPU program replaces, or the GPU stage's, such as
    /// `pipeline-failed`.
    Plan(&'static str),
}

impl RendererReason {
    /// Every reason a session carries, in the order the session's description lists them.
    pub const ALL: [Self; 3] = [Self::SurfacePending, Self::NoAdapter, Self::DeviceLost];

    /// The reasons only an export's result names, besides the session's and the GPU plan's codes.
    pub const EXPORT: [Self; 4] = [
        Self::Requested,
        Self::Refused,
        Self::AdapterMismatch,
        Self::Budget,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SurfacePending => "surface-pending",
            Self::NoAdapter => "no-adapter",
            Self::DeviceLost => "device-lost",
            Self::Requested => "requested",
            Self::Refused => "refused",
            Self::AdapterMismatch => "adapter-mismatch",
            Self::Budget => "tiles-budget",
            Self::Plan(code) => code,
        }
    }
}

impl Serialize for RendererReason {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RendererReason {
    /// A session's reason, the only reasons a renderer is read back with.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        const SESSION: [&str; 3] = [
            RendererReason::ALL[0].as_str(),
            RendererReason::ALL[1].as_str(),
            RendererReason::ALL[2].as_str(),
        ];
        let code = std::borrow::Cow::<str>::deserialize(deserializer)?;
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == code)
            .ok_or_else(|| serde::de::Error::unknown_variant(&code, &SESSION))
    }
}

/// Which renderer draws the desktop's picture on this machine, and why the reference does, as
/// `{record, reason}` (`docs/design/gpu-first.md`): `{record: "gpu", reason: null}` while the
/// desktop's photo surface can draw on its GPU, with `software: true` when that GPU is the
/// platform's software adapter, and `{record: "reference", reason}` while it cannot. An owner that draws nothing, `luxforge-json`'s, always reports the reference with no
/// reason: it has no GPU stage to fall back from.
///
/// The process hosting the owner reports it ([`OwnerHandle::report_renderer`], a desktop-internal
/// path), every client's session carries the owner's one value, and no method sets it, so no
/// client can claim a renderer the desktop does not have. A GPU record never has a reason; a
/// session read with one is refused.
///
/// An export's result names the renderer that rendered its file in the same shape: the GPU, or
/// the reference with the reason the GPU did not render it, `requested` when the export asked for
/// the reference, and no reason on an owner with no GPU provider ([`RendererReason`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RendererFields")]
pub struct Renderer {
    record: RendererRecord,
    reason: Option<RendererReason>,
    /// The GPU record's adapter is a software one, a rasterizer on the CPU such as lavapipe or
    /// WARP: written only when true, and only with the GPU record.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    software: bool,
}

/// [`Renderer`]'s fields as they are read, before the GPU record's missing reason is checked.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RendererFields {
    record: RendererRecord,
    reason: Option<RendererReason>,
    #[serde(default)]
    software: bool,
}

impl TryFrom<RendererFields> for Renderer {
    type Error = &'static str;

    fn try_from(fields: RendererFields) -> Result<Self, Self::Error> {
        match fields {
            RendererFields {
                record: RendererRecord::Gpu,
                reason: Some(_),
                ..
            } => Err("a GPU renderer has no reason"),
            RendererFields {
                record: RendererRecord::Reference,
                software: true,
                ..
            } => Err("only a GPU renderer draws on a software adapter"),
            RendererFields {
                record,
                reason,
                software,
            } => Ok(Self {
                record,
                reason,
                software,
            }),
        }
    }
}

impl Renderer {
    /// The desktop's photo surface draws on its GPU.
    pub const fn gpu() -> Self {
        Self {
            record: RendererRecord::Gpu,
            reason: None,
            software: false,
        }
    }

    /// The desktop's photo surface draws on its GPU stage through the platform's software adapter.
    pub const fn gpu_software() -> Self {
        Self {
            record: RendererRecord::Gpu,
            reason: None,
            software: true,
        }
    }

    /// The reference renderer draws the desktop's picture, for `reason`.
    pub const fn reference(reason: RendererReason) -> Self {
        Self {
            record: RendererRecord::Reference,
            reason: Some(reason),
            software: false,
        }
    }

    /// An owner that draws nothing: the reference renderer is its only renderer.
    pub const fn headless() -> Self {
        Self {
            record: RendererRecord::Reference,
            reason: None,
            software: false,
        }
    }

    /// Whether the GPU draws through a software adapter.
    pub fn software(self) -> bool {
        self.software
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
