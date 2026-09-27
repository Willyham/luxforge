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

/// The side of one spatial output tile the host streams a stage in: the grid a RAW-backed linear
/// source materialises its spatial layers in, and the byte domain's own point-sample tile.
pub const SPATIAL_TILE: u32 = 512;
