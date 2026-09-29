//! UI-independent JPEG decoding, non-destructive editing state, rendering and the JSON owner API.
pub mod activity;
pub mod analysis;
mod api;
mod artifacts;
mod atomic_file;
mod cancel;
pub mod capabilities;
/// One home for the sRGB transfer function, Rec. 709 luminance, the Oklab conversion, small 3×3
/// linear algebra and the Planckian locus, shared by every renderer and colour module.
pub mod colour;
#[cfg(test)]
mod command_contracts;
mod draft;
mod editor;
mod error;
/// JPEG export: capture metadata, the encoder and a publish that never replaces a file.
mod export;
pub mod jobs;
/// One persistent worker that runs the newest job, behind the preview, the analysis and the
/// desktop's clipping overlay.
pub mod latest;
/// The host's compiled mask and the component kinds this build can evaluate.
pub mod mask;
mod mask_field;
mod model;
mod modules;
/// The host's path primitives: the stored coordinate grid, decimation, the stroke a painting
/// action captures, and the content-addressed store those strokes live in.
pub mod path;
mod presets;
mod preview;
mod profile;
mod proxy;
mod render;
pub mod resources;
mod source;

// The public surface: what the desktop, `luxforge-json`, `luxforge-net`, `luxforge-testkit`, xtask
// and this crate's integration tests name through the crate root.
pub use activity::ActivitySnapshot;
pub use api::{
    ApiRequest, ApiResponse, ClientAuthority, ClientId, ClientSession, EventWake, EventsResult,
    LocalServer, MASK_MODE, MaskOverlayColour, MaskOverlayMode, OwnerHandle, POINTER_MODE,
    PreviewRequest, WorkspaceState, schemas, serve_json_lines_with,
};
pub use artifacts::ArtifactId;
pub use cancel::Cancel;
pub use capabilities::host::HostConfig;
pub use capabilities::redact::redact_params;
pub use draft::Draft;
pub use editor::{
    ActionResult, AssetRecord, DraftStamp, EditorService, EditorState, Evaluation, HistoryPage,
    LayerDescription, Lineage, LineageStep, MASK_FIELD, MutationOutcome, MutationResult,
    RecipeDescription, SkippedSetting, SourceKind, SourceTag, Version,
};
pub use error::{Error, ErrorKind, Preparation};
pub use model::{
    AssetId, COMPONENTS_PER_MASK, Component, ComponentId, ComponentMode, DraftId, EFFECT_FORMAT,
    EntryId, HistoryEntry, HistoryRow, JobId, Layer, LayerId, MASKS_PER_RECIPE, Mask, MaskId,
    Mutation, MutationRequest, Orientation, PresetId, RECIPE_FORMAT, Recipe, Snapshot, SnapshotId,
    Transform,
};
pub use modules::{
    ActionControl, ActionDescriptor, ActionInput, ActionPlan, ActionStyle, Availability,
    BASIC_EFFECT, BoxRect, CONTROLS_EFFECT, CROP_EFFECT, CanvasInteraction, ChoiceStyle,
    ColorOperation, ColorStyle, Control, ControlsModule, CropAspect, CropPayload, CropStage,
    CurveBackground, CurveChannel, CurveControl, Edge, EffectDescriptor, EffectStage, GroupControl,
    LayerReport, MAX_ANGLE, MIN_ANGLE, MIXER_EFFECT, ModuleDescriptor, ModuleLayout,
    ModuleRegistry, NumberControl, NumberStyle, ORIENTATION_EFFECT, OutputRect, PIXEL_EFFECT,
    PRESENCE_EFFECT, PROOF_GENERATE_PATH, PROOF_PALETTE, PROOF_PALETTE_PATH, ParameterDescriptor,
    ParameterKind, PickerControl, PointwiseColor, PresetsControl, Processing, Provider,
    RailDecoration, RawModule, RawPayload, Region, RegistryOptions, ResetAction, Stage,
    StageContext, StageQuestions, ToolModule, VIGNETTE_EFFECT, WhiteBalanceMode, check_parameters,
    check_value, gains_from_temperature_tint, guide_angle, insertion_index_among,
    largest_with_ratio_inside, palette_bytes, resolve_control, resolve_group_reset,
    temperature_tint_from_gains,
};
pub use presets::{
    MAX_PRESET_BYTES, PresetOrigin, PresetSummary, ReportCounts, USER_PRESET_GROUP, inspect_preset,
};
pub use preview::{
    ExactOutcome, HistorySelection, MaskCoverage, MaskOverlayOutcome, MaskOverlayRequest,
    PhaseOutcome, PreviewIntent, PreviewJob, PreviewPhase, PreviewQueue, PreviewResult,
    PreviewSource, ProxyOutcome, Queued, RegionOutcome, Zoom,
};
pub use proxy::{ProxyApproximation, ProxyBounds, ProxyIdentity};
pub use render::{
    ContentPoint, LinearSettings, Raster, RegionFrame, RenderContext, RenderOptions, RenderSource,
    Sample, StageSize, StageTransform, render, stage_transform,
};
pub use source::{LinearImage, SourceImage, open_source};

// The crate root paths the core itself uses.
pub(crate) use editor::{AnalysisPlan, AnalysisSelection, PixelSample};
pub(crate) use error::PreparationNeeds;
pub(crate) use model::{JobStatus, MAX_MASK_NAME, POINTS_PER_MASK, PixelReplace};
pub(crate) use modules::{
    ActionRef, IdentityKind, MAX_PRESET_NAME, MAX_SETTINGS_ACTIONS, MAX_SETTINGS_FIELDS, QueryRef,
    Superseded, lightroom_to_luxforge, valid_name,
};
pub(crate) use preview::PreviewSession;
pub(crate) use proxy::{ProxyCache, ProxyKey, ProxyPlan, ProxyWindow};
pub(crate) use render::{RegionRenderOutcome, Render, WhiteBalanceApproximation};
pub(crate) use source::{open_source_bytes, read_bounded_file};

// The crate root paths only the core's unit tests use.
#[cfg(test)]
pub(crate) use activity::ActivityBoard;
#[cfg(test)]
pub(crate) use api::ApiFailure;
#[cfg(test)]
pub(crate) use capabilities::redact::redact_request;
#[cfg(test)]
pub(crate) use model::MASK_BYTES_PER_RECIPE;
#[cfg(test)]
pub(crate) use modules::{
    APPLY_PRESET, BasicModule, CapabilitiesProofModule, CapabilityModule, ChoiceControl,
    ControlVariant, ExactGeometry, LayerUpdate, MAX_COLOR_UNITS, NewLayer, PROOF_PALETTE_GAINS,
    PROOF_TASK, PresenceModule, RangeControl, ToggleControl,
};
#[cfg(test)]
pub(crate) use render::ProxyRegionPlan;
