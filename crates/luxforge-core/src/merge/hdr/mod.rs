//! The HDR merge provider's numerical core, in the order a merge uses it:
//!
//! 1. [`frame`]: the typed per-input metadata and [`validate_bracket`], which refuses an
//!    unsuitable request with a typed [`Refusal`] (RAW only, one body and recording mode, one
//!    colour interpretation, dimensions, aperture, ISO and focal length, no RAW+JPEG pair of one
//!    exposure, 2–9 inputs).
//! 2. [`exposure`]: [`resolve_exposures`] picks the reference frame and each input's effective
//!    EV offset from its settings or its recorded bias, never both, with per-input overrides;
//!    when the metadata is absent or contradictory it returns a [`FitPlan`], which a bounded set
//!    of aligned [`FitSamples`] completes with a measured offset, its residual and confidence, or
//!    refuses as ambiguous or insufficient. Every offset records its [`ExposureOrigin`].
//! 3. [`clipping`]: [`SensorClipModel`] derives each developed pixel's [`SensorLevel`], its
//!    sensor-clipping evidence, from the developed linear planes and the RAW interpretation.
//! 4. [`fusion`]: [`fuse_rect`] fuses one rectangle in reference coordinates from every input's
//!    aligned linear samples and levels, with an optional [`MotionSelection`], into float32
//!    linear sRGB/D65 at the reference's brightness, and reports [`FusionDiagnostics`] that
//!    aggregate across rectangles.
//!
//! The equations, weights, tolerances and sample bounds are recorded in the design's
//! "Exposure resolution and fusion equations".

pub mod clipping;
pub mod exposure;
pub mod frame;
pub mod fusion;

pub use clipping::{BAYER_INPUT_CEILING, SensorClipModel};
pub use exposure::{
    ExposureFit, ExposureOptions, ExposureOrigin, ExposureResolution, FitPlan, FitSamples,
    MetadataExposure, MetadataSource, ReferenceOrigin, ResolvedExposure, ResolvedExposures,
    resolve_exposures,
};
pub use frame::{
    ExposureSettings, FrameMetadata, FrameRect, MAX_INPUTS, MIN_INPUTS, MotionSelection, Recording,
    Refusal, SensorLayout, SensorLevel, validate_bracket,
};
pub use fusion::{
    FusedRect, FusionDiagnostics, FusionParameters, Limitations, PixelLimits, RangeOutcome,
    fuse_rect,
};

#[cfg(test)]
mod hdr_merge_fusion;
