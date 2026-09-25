//! UI-independent JPEG decoding, non-destructive editing state, rendering and the JSON owner API.
pub mod activity;
pub mod analysis;
mod api;
pub mod artifacts;
mod atomic_file;
pub mod capabilities;
/// One home for the sRGB transfer function, Rec. 709 luminance, the Oklab conversion, small 3×3
/// linear algebra and the Planckian locus, shared by every renderer and colour module.
pub mod colour;
#[cfg(test)]
mod command_contracts;
mod draft;
mod editor;
mod error;
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
pub use activity::{Activity, ActivityBoard, ActivitySnapshot, ActivitySpec};
pub use api::*;
pub use artifacts::ArtifactId;
pub use capabilities::{
    host::HostConfig,
    redact::{redact_params, redact_request},
};
pub use draft::Draft;
pub use editor::*;
pub use error::{Error, ErrorKind, Preparation, PreparationNeeds};
pub use model::*;
pub use modules::*;
pub use presets::*;
pub use preview::*;
pub use proxy::{ProxyApproximation, ProxyBounds, ProxyCache, ProxyIdentity, ProxyKey, ProxyPlan};
pub use render::{
    Cancel, ContentPoint, LinearImage, LinearSettings, Raster, Sample, ScratchBudget,
    SpatialBudget, StageSize, StageTransform, WhiteBalanceApproximation, extents, render,
    render_cancellable, render_linear, render_linear_cancellable, sample, sample_linear,
    stage_transform,
};
pub use source::{SourceImage, open_source};
pub(crate) use source::{open_source_bytes, read_bounded_file};
