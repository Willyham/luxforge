//! The panorama provider's library (`docs/design/panorama-merge.md`, "Panorama processing
//! contract").
//!
//! - [`frames`] is a panorama's working pixels: each RAW or JPEG frame read once, developed (a
//!   RAW with its detected lens profile baked, a JPEG into linear sRGB/D65 through its colour
//!   profile) and streamed by bands into the job's spool, its registration proxy and its colour
//!   proxy.
//! - [`suitability`] is what frames must say of themselves to be a panorama, from their headers
//!   and again once decoded, and words every panorama refusal by the frames' file names.
//! - [`stitching`] is the seam between the job and the stitch: what the job asks of a
//!   [`stitching::Stitcher`] once registration has placed the frames.
//! - [`job`] is the panorama merge job the merge lane runs.
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
//!
//! Stitching ([`stitch`]):
//!
//! 4. [`plan_stitch`] lays the output out from the registration and the frames' colour proxies:
//!    the [`projection`], the bounding rectangle and scale ([`layout`]), per-frame gains
//!    ([`photometric`]), seams by minimum cuts ([`seams`]), the two-band blend's correction field
//!    ([`blend`]), Boundary Warp's mesh ([`warp`]), the crop and the stack order, and draws the
//!    merge preview from the proxies ([`StitchPlan::preview`]).
//! 5. [`StitchPlan::render_chunk`] draws any rectangle of the result from windows of the frames'
//!    full-resolution pixels, bit-identically for any split into chunks.

pub mod blend;
pub mod bundle;
pub mod camera;
pub mod features;
// The job reads frames through this; a few counts it reports (the spool's bytes and clipped
// pixels, a frame's optics) are read only by its tests.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod frames;
pub(crate) mod job;
pub mod layout;
mod linalg;
pub mod matching;
pub mod photometric;
pub(crate) mod preview;
pub mod projection;
pub mod proxy;
pub mod refusal;
pub mod registration;
pub mod seams;
pub mod stitch;
pub(crate) mod stitching;
pub mod suitability;
pub mod warp;

pub use camera::{Lens, MAX_RADIAL, Rotation};
pub use features::{Feature, FrameFeatures, MAX_FEATURES};
pub use frames::BakedLensProfile;
pub use layout::OutputGeometry;
pub use photometric::{FrameGain, GainMode};
pub use projection::{Field, Projection, ProjectionChoice};
pub use proxy::{MAX_PROXY_SIDE, PanoramaProxy, ProxyBuilder, ProxyRows};
pub use refusal::Refusal;
pub use registration::{
    FocalOrigin, FocalPrior, FrameEvidence, Levelling, MAX_FRAMES, MAX_PAIRS, MIN_FRAMES,
    PairEvidence, PairStatus, PanoramaRegistration, RadialMode, RadialOrigin, ReferenceOrigin,
    RegistrationDiagnostics, RegistrationOptions, RegistrationStage, register,
    register_with_progress,
};
pub use stitch::{
    ChunkStats, Contribution, FrameOutline, FrameWindow, FrameWindows, PanoramaOverlays,
    PanoramaPreview, PlanGrid, SeamLength, StitchDiagnostics, StitchOptions, StitchPlan,
    StitchStage, StitchedChunk, WindowReader, plan_stitch, plan_stitch_with_progress,
};
pub use warp::WarpMesh;

#[cfg(test)]
mod panorama_registration;
#[cfg(test)]
mod panorama_stitch;
