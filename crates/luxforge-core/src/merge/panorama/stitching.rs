//! The seam between the panorama job and the stitch (`docs/design/panorama-merge.md`, "Projection
//! and output size", "Exposure, colour and seams", "Boundary Warp and the crop"): what the job asks
//! of a [`Stitcher`] once registration has placed the frames, and nothing of how it answers.
//!
//! 1. **Plan** ([`Stitcher::plan`]) from the registration and the frames' colour proxies alone:
//!    the resolved projection and field of view, the output scale (1 at the reference's centre
//!    pixel, below 1 only to fit [`OutputLimits`]), the bounding rectangle and the crop the result
//!    is, the gains, seams and Boundary Warp mesh ([`PanoramaStitch`]), and what rendering holds.
//!    A stitch refusal (`ExcessiveFieldOfView`, `OutputTooLarge`, `InsufficientCoverage`) is a
//!    typed error naming frames by request position (`data.kind` `panorama`), as registration's
//!    are; the job words it by file name.
//! 2. **Windows** ([`Stitcher::windows`]): the bounded rectangle of each frame an output chunk
//!    reads, so the job reads exactly those back from the spool.
//! 3. **Render a chunk** ([`Stitcher::render_chunk`]) of the result from those windows, straight
//!    into the writer's planar chunk buffer. Output is the same for any split into chunks.
//! 4. **Preview** ([`Stitcher::preview`]): the whole stitch at preview scale from the colour
//!    proxies, with the overlays' data in the picture's pixels.
//!
//! Frames are by request position throughout; the job turns them into the stack's order.
//!
//! This build's production stitch is [`production`]: none yet, so a panorama merge is refused
//! `not-ready` at admission. A test supplies its own.
use crate::{
    Cancel, Error,
    catalog_types::{PanoramaPreviewOverlays, PanoramaProjection, PanoramaStitch},
    merge::{
        hdr::{ColourProxy, PixelRect, SourceWindow},
        panorama::PanoramaRegistration,
    },
};
use std::{any::Any, sync::Arc};

#[cfg(test)]
#[path = "stitching_simple.rs"]
pub(crate) mod simple;

/// The largest output a stitch may plan: the paged derived-source limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct OutputLimits {
    pub max_side: u32,
    pub max_pixels: u64,
}

impl OutputLimits {
    /// A derived source's limits (`crate::source::PAGED_DERIVED_SOURCE`).
    pub(crate) const DERIVED: Self = Self {
        max_side: crate::source::PAGED_DERIVED_SOURCE.max_side,
        max_pixels: crate::source::PAGED_DERIVED_SOURCE.max_pixels,
    };

    /// Whether a `width × height` output is within them.
    pub(crate) fn fits(&self, width: u32, height: u32) -> bool {
        width.max(height) <= self.max_side
            && u64::from(width) * u64::from(height) <= self.max_pixels
    }
}

/// What a stitch is planned from, every frame by request position. The tests' stitch reads only
/// what it needs of it.
#[cfg_attr(test, allow(dead_code))]
pub(crate) struct StitchRequest<'a> {
    pub registration: &'a PanoramaRegistration,
    /// Each frame's colour proxy (`crate::merge::hdr::ColourProxy`): its mean linear samples and
    /// largest clipping levels on a grid of at most 1,024 a side.
    pub colour: &'a [ColourProxy],
    /// The frames are RAW (one gain a frame) rather than JPEG (one gain a channel).
    pub raw: bool,
    /// Each frame's scale to the reference's exposure from its settings: the photometric fit's
    /// starting point (1 for JPEG frames).
    pub exposure_scales: &'a [f64],
    pub projection: PanoramaProjection,
    /// 0 to 100.
    pub boundary_warp: u8,
    pub limits: OutputLimits,
}

/// A planned stitch: the result's size, what the provenance records of it, what rendering holds,
/// and the stitcher's own state.
#[derive(Clone)]
pub(crate) struct StitchPlan {
    /// The result: the crop, at the plan's scale.
    pub width: u32,
    pub height: u32,
    /// Per frame by request position.
    pub record: PanoramaStitch,
    /// What the plan holds while it is kept: seam maps, the warp mesh.
    pub plan_bytes: u64,
    /// What rendering one chunk holds beside the frames' windows.
    pub chunk_bytes: u64,
    pub inner: Arc<dyn Any + Send + Sync>,
}

impl std::fmt::Debug for StitchPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StitchPlan")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("record", &self.record)
            .finish_non_exhaustive()
    }
}

/// One frame's window as a chunk reads it: the frame's request position and its samples.
pub(crate) struct ChunkWindow<'a> {
    pub frame: usize,
    pub window: SourceWindow<'a>,
}

/// The preview's picture of the whole stitch, linear, and its overlays in its pixels (frames by
/// request position).
pub(crate) struct PreviewStitch {
    pub width: usize,
    pub height: usize,
    /// Planar linear sRGB; uncovered pixels are 0.
    pub rgb: [Vec<f32>; 3],
    pub overlays: PanoramaPreviewOverlays,
}

impl PreviewStitch {
    /// The bytes a preview picture of at most `side` pixels a side holds.
    pub(crate) fn bytes_for(side: u32) -> u64 {
        u64::from(side) * u64::from(side) * 12
    }
}

/// The stitch: see the module documentation.
pub(crate) trait Stitcher: Send + Sync {
    /// Plan the stitch of the registered frames.
    fn plan(&self, request: &StitchRequest<'_>, cancel: &Cancel) -> Result<StitchPlan, Error>;

    /// The rectangle of each frame (full-resolution pixels) the result's `chunk` (in the result's
    /// pixels) reads, by request position; a frame it does not read is absent.
    fn windows(
        &self,
        plan: &StitchPlan,
        chunk: PixelRect,
    ) -> Result<Vec<(usize, PixelRect)>, Error>;

    /// Render `chunk` of the result from `windows` (those [`Self::windows`] named) into `out`,
    /// three planes of `chunk`'s pixels, red then green then blue.
    fn render_chunk(
        &self,
        plan: &StitchPlan,
        chunk: PixelRect,
        windows: &[ChunkWindow<'_>],
        out: &mut [f32],
        cancel: &Cancel,
    ) -> Result<(), Error>;

    /// The preview's picture of the stitch, at most `side` pixels a side, from the colour proxies.
    fn preview(
        &self,
        plan: &StitchPlan,
        colour: &[ColourProxy],
        side: u32,
        cancel: &Cancel,
    ) -> Result<PreviewStitch, Error>;
}

/// This build's panorama stitch: none until the stitch is built (`docs/design/panorama-merge.md`,
/// TASK-008), so the merge lane refuses panorama merges `not-ready` naming it.
pub(crate) fn production() -> Option<Arc<dyn Stitcher>> {
    None
}

/// Why this build cannot stitch, while [`production`] is none.
pub(crate) const UNWIRED: &str = "this build's panorama stitch is not built yet";
