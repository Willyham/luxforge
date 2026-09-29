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
// and this crate's integration tests name through the crate root, and every type a public item's
pub use activity::ActivitySnapshot;
pub use api::{
    ApiEvent, ApiFailure, ApiRequest, ApiResponse, ClientAuthority, ClientId, ClientSession,
    EventWake, EventsResult, LocalServer, MASK_MODE, MaskOverlayColour, MaskOverlayMode,
    OwnerHandle, POINTER_MODE, PreviewRequest, WorkspaceState, schemas, serve_json_lines_with,
};
pub use artifacts::{ArtifactId, ArtifactTable, PreparedArtifact};
pub use cancel::Cancel;
pub use capabilities::context::ModuleContext;
pub use capabilities::host::HostConfig;
pub use capabilities::redact::redact_params;
pub use draft::{Draft, DraftTarget, declared_target};
pub use editor::{
    ActionResult, AssetPage, AssetRecord, AssetSummary, DraftStamp, EditorService, EditorState,
    Evaluation, HistoryPage, LayerDescription, Lineage, LineageStep, MASK_FIELD, MutationOutcome,
    MutationResult, PixelInput, PixelSample, RawInterpretation, RecipeDescription, SkippedSetting,
    SourceKind, SourceTag, Version,
};
pub use error::{Error, ErrorKind, Preparation, PreparationNeeds};
pub use export::CaptureMetadata;
pub use model::{
    AssetId, COMPONENTS_PER_MASK, Component, ComponentId, ComponentMode, DraftId, EFFECT_FORMAT,
    EntryId, HistoryEntry, HistoryRow, JobId, Layer, LayerId, MASKS_PER_RECIPE, Mask, MaskId,
    Mutation, MutationRequest, Orientation, PresetId, RECIPE_FORMAT, Recipe, Snapshot, SnapshotId,
    Transform,
};
pub use modules::{
    ActionControl, ActionDescriptor, ActionInput, ActionPlan, ActionRef, ActionStyle, Availability,
    BASIC_EFFECT, BoxRect, CONTROLS_EFFECT, CROP_EFFECT, CanvasInteraction, CapabilityModule,
    ChoiceControl, ChoiceStyle, ColorControl, ColorOperation, ColorStyle, Control, ControlVariant,
    Controls, ControlsModule, CropAspect, CropPayload, CropStage, CurveBackground, CurveChannel,
    CurveControl, Edge, EffectDescriptor, EffectStage, ExactGeometry, FieldPatch, FieldPatchModule,
    GroupControl, IdentityKind, LayerEdit, LayerReport, LayerUpdate, MAX_ANGLE, MIN_ANGLE,
    MIXER_EFFECT, ModuleDescriptor, ModuleLayout, ModuleRegistry, NewLayer, NumberControl,
    NumberStyle, ORIENTATION_EFFECT, OutputRect, PIXEL_EFFECT, PRESENCE_EFFECT,
    PROOF_GENERATE_PATH, PROOF_PALETTE, PROOF_PALETTE_PATH, ParameterDescriptor, ParameterKind,
    PickerControl, PointwiseColor, PresetsControl, Processing, Provider, QueryRef, RailDecoration,
    RangeControl, RawModule, RawPayload, Region, RegistryOptions, Resample, ResetAction,
    ResolvedControl, ResolvedReset, SpatialOperation, Spec, Stage, StageContext, StageQuestions,
    TaskControl, ToggleControl, ToolModule, VIGNETTE_EFFECT, Values, WhiteBalanceMode,
    check_parameters, check_value, gains_from_temperature_tint, guide_angle, insertion_index_among,
    largest_with_ratio_inside, palette_bytes, resolve_control, resolve_group_reset,
    temperature_tint_from_gains,
};
pub use presets::{
    ImportReport, ImportedPreset, MAX_PRESET_BYTES, MappedSetting, PresetOrigin, PresetRecord,
    PresetSummary, ReportCounts, ReportedSetting, USER_PRESET_GROUP, inspect_preset,
};
pub use preview::{
    AssetSelection, ExactOutcome, HistorySelection, MAX_SELECTIONS, MaskCoverage,
    MaskOverlayOutcome, MaskOverlayRequest, PhaseOutcome, PreviewIntent, PreviewJob, PreviewPhase,
    PreviewQueue, PreviewResult, PreviewSession, PreviewSource, ProxyOutcome, Queued,
    RegionOutcome, ViewState, Zoom,
};
pub use proxy::{ProxyApproximation, ProxyBounds, ProxyIdentity, ProxyPlan};
pub use render::{
    ContentPoint, LinearSettings, Raster, RegionFrame, Render, RenderContext, RenderOptions,
    RenderSource, Sample, ScratchBudget, StageSize, StageTransform, WhiteBalanceApproximation,
    render, stage_transform,
};
pub use source::{LinearImage, SourceImage, open_source};

// The crate root paths the core itself uses.
pub(crate) use editor::{AnalysisPlan, AnalysisSelection};
pub(crate) use model::{JobStatus, MAX_MASK_NAME, POINTS_PER_MASK, PixelReplace};
pub(crate) use modules::{
    MAX_PRESET_NAME, MAX_SETTINGS_ACTIONS, MAX_SETTINGS_FIELDS, Superseded, lightroom_to_luxforge,
    valid_name,
};
pub(crate) use proxy::{ProxyCache, ProxyKey, ProxyWindow};
pub(crate) use render::RegionRenderOutcome;
pub(crate) use source::{open_source_bytes, read_bounded_file};

// The crate root paths only the core's unit tests use.
#[cfg(test)]
pub(crate) use activity::ActivityBoard;
#[cfg(test)]
pub(crate) use model::MASK_BYTES_PER_RECIPE;
#[cfg(test)]
pub(crate) use modules::{
    APPLY_PRESET, BasicModule, CapabilitiesProofModule, MAX_COLOR_UNITS, PROOF_PALETTE_GAINS,
    PresenceModule,
};
#[cfg(test)]
pub(crate) use render::ProxyRegionPlan;
