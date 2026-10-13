//! The HDR merge provider's numerical core, in the order a merge uses it:
//!
//! 1. [`frame`]: the typed per-input metadata and [`validate_bracket`], which refuses an
//!    unsuitable request with a typed [`Refusal`] (RAW only, one body and recording mode, one
//!    colour interpretation, dimensions, aperture and focal length within a rounding tolerance,
//!    no RAW+JPEG pair of one exposure, 2–9 inputs; ISO may vary).
//! 2. [`exposure`]: [`resolve_exposures`] picks the reference frame and each input's effective
//!    EV offset from its settings or its recorded bias, never both, with per-input overrides;
//!    when the metadata is absent or contradictory it returns a [`FitPlan`], which a bounded set
//!    of aligned [`FitSamples`] completes with a measured offset, its residual and confidence, or
//!    refuses as ambiguous or insufficient. Every offset records its [`ExposureOrigin`].
//! 3. [`clipping`]: [`SensorClipModel`] derives each developed pixel's [`SensorLevel`], its
//!    sensor-clipping evidence, from the developed linear planes and the RAW interpretation.
//! 4. [`proxy`]: each developed input, as it is developed, reduces to a bounded
//!    [`AnalysisProxy`] (log luminance and levels, at most 1024 a side), so no two full frames
//!    are held for analysis.
//! 5. [`align`]: [`register`] fits each input's rigid motion to the reference from the proxies,
//!    exposure invariantly, with [`AlignmentOptions`] (alignment and deghosting, both automatic
//!    by default), or checks the framing with alignment off; unalignable inputs are refused.
//! 6. [`coverage`]: [`common_coverage`] is the largest rectangle every aligned input recorded,
//!    the result's frame.
//! 7. [`deghost`]: the [`AlignedProxies`] feed the exposure fit and [`detect_motion`], whose
//!    [`MotionMap`] shows each moving region from one exposure and renders any rectangle's
//!    [`MotionSelection`].
//! 8. [`resample`]: [`resample_rect`] brings one input's bounded [`SourceWindow`] into reference
//!    coordinates for one rectangle, the merge's single resample.
//! 9. [`fusion`]: [`fuse_rect`] fuses one rectangle in reference coordinates from every input's
//!    aligned linear samples and levels, with an optional [`MotionSelection`], into float32
//!    linear sRGB/D65 at the reference's brightness, and reports [`FusionDiagnostics`] that
//!    aggregate across rectangles.
//! 10. [`preview`]: the merge preview's picture, drawn at proxy scale from each input's
//!     [`ColourProxy`] through the same resample and fusion ([`fuse_preview`]), its display
//!     transform and JPEG ([`encode_preview_jpeg`]), and its deghost overlay
//!     ([`motion_overlays`]).
//!
//! The equations, weights, tolerances and sample bounds are recorded in the design's
//! "Exposure resolution and fusion equations" and "Alignment, coverage and motion equations".

pub mod align;
pub mod clipping;
pub mod coverage;
pub mod deghost;
pub mod exposure;
pub mod frame;
pub mod fusion;
pub mod preview;
pub mod proxy;
pub mod resample;

pub use align::{
    AlignmentEvidence, AlignmentMode, AlignmentOptions, DeghostMode, InputAlignment, PixelRect,
    Registration, RigidTransform, register,
};
pub use clipping::{BAYER_INPUT_CEILING, SensorClipModel};
pub use coverage::{Coverage, common_coverage};
pub use deghost::{
    AlignedProxies, MotionDiagnostics, MotionMap, MotionRect, MotionRegion, detect_motion,
};
pub use exposure::{
    ExposureFit, ExposureOptions, ExposureOrigin, ExposureResolution, FitPlan, FitSamples,
    MetadataExposure, MetadataSource, ReferenceOrigin, ResolvedExposure, ResolvedExposures,
    resolve_exposures,
};
pub use frame::{
    ExposureSettings, FOCAL_TOLERANCE_MM, FOCAL_TOLERANCE_RELATIVE, FrameMetadata, FrameRect,
    MAX_INPUTS, MIN_INPUTS, MotionSelection, Recording, Refusal, SensorLayout, SensorLevel,
    validate_bracket,
};
pub use fusion::{
    FusedRect, FusionDiagnostics, FusionParameters, Limitations, PixelLimits, RangeOutcome,
    fuse_rect,
};
pub use preview::{
    ColourProxy, ColourProxyBuilder, MotionOverlay, PreviewPicture, display_pixel,
    encode_preview_jpeg, fuse_preview, max_residual_px, motion_overlays, preview_proxy_side,
    preview_rect, preview_scratch_bytes, proxy_transform,
};
pub use proxy::{AnalysisProxy, ProxyBuilder};
pub use resample::{AlignedRect, SourceWindow, resample_rect, source_window};

/// The synthetic RAW bracket generator, compiled in from its one source for the core's tests: the
/// core cannot name `luxforge-testkit`, which depends on it.
#[cfg(test)]
#[allow(dead_code)]
#[path = "../../../../luxforge-testkit/src/hdr_bracket.rs"]
pub(crate) mod hdr_bracket;
#[cfg(test)]
mod hdr_merge_alignment;
#[cfg(test)]
mod hdr_merge_fusion;
#[cfg(test)]
mod merge_preview;
