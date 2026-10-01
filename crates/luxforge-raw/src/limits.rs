//! Shared admission and rendering bounds.
//!
//! The RAW-only admission bounds are used by the adapter, catalog and callers. The cross-crate
//! bounds below them are declared here because `luxforge-core` depends on `luxforge-raw`, not the
//! reverse: this is the one module both crates can import from, so a threshold, frame limit or tile
//! size used by more than one call site across the two crates has exactly one declaration, whatever
//! crate first needed it.
/// Maximum encoded RAW source bytes (512 MiB).
pub const MAX_SOURCE_BYTES: usize = 512 * 1024 * 1024;
/// Maximum full sensor pixels (128 million).
pub const MAX_PIXELS: usize = 128_000_000;
/// Maximum sensor side; also bounded by the pixel count.
pub const MAX_SIDE: u32 = 16_384;
/// Maximum one planar RGB float allocation (1.5 GiB), not a process RSS limit.
pub const MAX_RGB_BYTES: usize = 1536 * 1024 * 1024;
/// The largest development whose planes the editor retains beside its current one (600 MiB), so
/// that switching between two entries at different white balances redevelops neither: a 40.9 MP
/// X100VI development (468 MiB of planes) fits, a development at the [`MAX_PIXELS`] limit does not.
pub const RETAINED_DEVELOPMENT_BYTES: usize = 600 * 1024 * 1024;

/// Above this many pixels a per-pixel pass that is not a rendering pass moves from a serial loop to
/// the shared Rayon pool: the RAW development passes (the camera-matrix conversion and the DNG
/// corrections), a spatial operation's global-estimate reduction, the analysis reducer and the
/// overlays. A rendering pass reads its own threshold from [`parallel_pixels`].
pub const PARALLEL_PIXELS: u64 = 1_000_000;

/// A rendering pass's kind, which chooses the pixel count at and past which the pass runs on the
/// shared Rayon pool ([`parallel_pixels`]). Each threshold is measured with one unit per case,
/// serial against pooled at 0.025 to 2 megapixels on the M4 (`render::parallel`'s
/// `parallel_break_even_per_pass`; `docs/specs/performance.md`, "Per-pass parallel thresholds"):
/// it is the smallest size measured at which pooling won in every case of its kind. The ratios
/// below are pooled time over serial time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderPass {
    /// A segment's exact geometry, counted over the segment's pixels.
    Transform,
    /// A segment's colour runs with one or two colour units, counted over the rows they reach.
    Colour,
    /// A segment's colour runs with at least three colour units or a mask, counted over the rows
    /// they reach.
    HeavyColour,
    /// An interpolating resample, counted over the output pixels it writes.
    Resample,
    /// A nonlinear warp chain; threshold provisional until photo-sized measurement.
    Warp,
    /// A spatial operation's tiles, counted over its stage.
    Spatial,
    /// A proxy source's box downscale, counted over the source pixels it reads.
    Proxy,
}

/// [`RenderPass::Transform`]: a quarter turn is 0.61 to 0.73 at 0.5 MP and 1.24 to 1.26 at 0.25 MP.
pub const PARALLEL_TRANSFORM_PIXELS: u64 = 500_000;

/// [`RenderPass::Colour`]: one Exposure unit is 0.49 to 0.58 at 0.1 MP on bytes (0.34 to 0.77 on
/// linear planes) and 0.89 to 0.93 on bytes at 0.05 MP.
pub const PARALLEL_COLOUR_PIXELS: u64 = 100_000;

/// [`RenderPass::HeavyColour`]: a full Basic layer's four units are 0.31 to 0.37 on bytes and 0.45
/// to 0.77 on linear planes at 0.025 MP, the smallest size measured. A mask is counted here too,
/// as before, and was not measured on its own.
pub const PARALLEL_HEAVY_COLOUR_PIXELS: u64 = 25_000;

/// [`RenderPass::Resample`]: a 10 degree crop is 0.59 to 0.60 at 0.14 MP of output, 0.75 at
/// 0.07 MP and 1.14 at 0.03 MP.
pub const PARALLEL_RESAMPLE_PIXELS: u64 = 100_000;

/// Provisional: use the resample gate until warp-specific distributions are measured.
pub const PARALLEL_WARP_PIXELS: u64 = PARALLEL_RESAMPLE_PIXELS;

/// [`RenderPass::Spatial`]: at 0.25 MP each Presence unit's fastest runs are 0.21 to 0.88 and all
/// three together 0.33 to 0.34 at the p50; at 0.1 MP Texture or Dehaze alone breaks even or loses.
pub const PARALLEL_SPATIAL_PIXELS: u64 = 250_000;

/// [`RenderPass::Proxy`]: a box downscale to a third is 0.56 to 0.78 at 0.5 MP of source read; up
/// to 0.25 MP it reads one band, so pooling changes nothing.
pub const PARALLEL_PROXY_PIXELS: u64 = 500_000;

/// The pixel count at and past which a rendering pass of this kind runs on the shared Rayon pool:
/// the one table every rendering pass's parallel gate reads.
pub const fn parallel_pixels(pass: RenderPass) -> u64 {
    match pass {
        RenderPass::Transform => PARALLEL_TRANSFORM_PIXELS,
        RenderPass::Colour => PARALLEL_COLOUR_PIXELS,
        RenderPass::HeavyColour => PARALLEL_HEAVY_COLOUR_PIXELS,
        RenderPass::Resample => PARALLEL_RESAMPLE_PIXELS,
        RenderPass::Warp => PARALLEL_WARP_PIXELS,
        RenderPass::Spatial => PARALLEL_SPATIAL_PIXELS,
        RenderPass::Proxy => PARALLEL_PROXY_PIXELS,
    }
}

/// The largest RGBA8 frame any evaluated render, proxy or linear-to-byte conversion may allocate
/// (512 MiB).
pub const MAX_FRAME_BYTES: u64 = 512 * 1024 * 1024;

/// The side of the spatial output tiles the host streams a stage in when the operation's summed
/// halo is at most [`SPATIAL_WIDE_HALO`]: the grid its render, its point samples and a windowed
/// proxy's origin all use.
pub const SPATIAL_TILE: u32 = 512;

/// The side of the spatial output tiles for an operation whose summed halo is past
/// [`SPATIAL_WIDE_HALO`], where a 512 px tile would recompute more of the halo around every tile
/// than the larger tile costs.
pub const SPATIAL_WIDE_TILE: u32 = 1024;

/// The summed halo, in pixels at the operation's stage, past which a spatial operation runs in
/// [`SPATIAL_WIDE_TILE`] tiles. Measured on the M4 with Presence: past it the wide tile rendered
/// every stack 17 to 51% faster; up to it at most 14% faster, and Texture alone slower, while a
/// point sample through the wide tile cost three to four times as much (`docs/decisions.md`).
pub const SPATIAL_WIDE_HALO: u32 = 128;

/// The side of the tiles a spatial operation with this summed halo at its stage runs in: the one
/// tile-size rule, which the render, the point sample and the windowed proxy all ask.
pub const fn spatial_tile(summed_halo: u32) -> u32 {
    if summed_halo > SPATIAL_WIDE_HALO {
        SPATIAL_WIDE_TILE
    } else {
        SPATIAL_TILE
    }
}
