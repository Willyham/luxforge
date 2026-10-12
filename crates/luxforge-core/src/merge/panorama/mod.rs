//! The panorama provider's library (`docs/design/panorama-merge.md`, "Panorama processing
//! contract").
//!
//! - [`frames`] is a panorama's working pixels: each RAW or JPEG frame read once, developed (a
//!   RAW with its detected lens profile baked, a JPEG into linear sRGB/D65 through its colour
//!   profile) and streamed by bands into the job's spool and its analysis proxy.
//!
//! The rest is the numerical core. Nothing there reads a file, touches the catalog or schedules
//! work. Registration, in the order a merge uses it:
//!
//! 1. [`proxy`]: each developed frame streams, band by band, into a bounded [`PanoramaProxy`] of
//!    its `log2` luminance (at most 1,536 a side).
//! 2. [`features`]: [`FrameFeatures::detect`] finds at most 4,000 oriented multi-scale corners on
//!    a proxy, each with an exposure-invariant binary descriptor.
//! 3. [`registration`]: [`register`] matches every pair ([`matching`]), verifies each with a
//!    RANSAC homography and the rotating camera, fits one rotation a frame with a shared focal
//!    length and radial term ([`camera`]) by robust bundle adjustment ([`bundle`]), checks
//!    connectivity and same views, levels the world and picks the reference, or refuses with a
//!    typed [`Refusal`] naming the frames.

pub mod bundle;
pub mod camera;
pub mod features;
// The panorama merge job reads its frames through this; until it does, only the tests here do.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod frames;
mod linalg;
pub mod matching;
pub mod proxy;
pub mod refusal;
pub mod registration;

pub use camera::{Lens, MAX_RADIAL, Rotation};
pub use features::{Feature, FrameFeatures, MAX_FEATURES};
pub use proxy::{MAX_PROXY_SIDE, PanoramaProxy, ProxyBuilder, ProxyRows};
pub use refusal::Refusal;
pub use registration::{
    FocalOrigin, FocalPrior, FrameEvidence, Levelling, MAX_FRAMES, MAX_PAIRS, MIN_FRAMES,
    PairEvidence, PairStatus, PanoramaRegistration, RadialMode, RadialOrigin, ReferenceOrigin,
    RegistrationDiagnostics, RegistrationOptions, RegistrationStage, register,
    register_with_progress,
};

#[cfg(test)]
mod panorama_registration;
