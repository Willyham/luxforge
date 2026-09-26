//! Shared RAW-only admission bounds, used by the adapter, catalog and callers.
/// Maximum encoded RAW source bytes (512 MiB).
pub const MAX_SOURCE_BYTES: usize = 512 * 1024 * 1024;
/// Maximum full sensor pixels (128 million).
pub const MAX_PIXELS: usize = 128_000_000;
/// Maximum sensor side; also bounded by the pixel count.
pub const MAX_SIDE: u32 = 16_384;
/// Maximum one planar RGB float allocation (1.5 GiB), not a process RSS limit.
pub const MAX_RGB_BYTES: usize = 1536 * 1024 * 1024;
