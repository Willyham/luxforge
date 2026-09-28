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

/// Above this many pixels a per-pixel pass moves from a serial loop to the shared Rayon pool: the
/// point past which per-row or per-chunk parallel dispatch is paid back by the work it saves.
/// Shared by every pass that picks its parallel path this way: the byte rasterizer, the proxy
/// build, the camera-matrix conversion, the analysis reducer and the RAW DNG corrections.
pub const PARALLEL_PIXELS: u64 = 1_000_000;

/// A sub-[`PARALLEL_PIXELS`] pass with enough independent colour work (several colour units, or a
/// mask) to pay for the shared Rayon pool below the ordinary threshold; smaller than this, even
/// that work stays serial.
pub const PARALLEL_HEAVY_COLOUR_PIXELS: u64 = 256 * 1024;

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
