//! RAW source/development admission bounds and the shared non-rendering parallel threshold.
//! Renderer-only frame, pass and tile policy lives in core rendering.
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

/// The largest embedded image `EmbeddedPreviews::extract` hands over (64 MiB); a caller's own byte
/// limit goes below it. It bounds LibRaw's thumbnail allocation, checked from the list item before
/// LibRaw allocates, and the copy returned, so one extraction holds at most twice this at once.
/// Camera JPEG previews are a few megabytes even at full size (at most about 12 MB in the
/// inventory, `docs/research/embedded-previews.md`), and 64 MiB of 8-bit RGB is a 22 MP bitmap.
pub const MAX_EMBEDDED_IMAGE_BYTES: usize = 64 * 1024 * 1024;
/// The largest read budget an embedded-preview handle accepts (128 MiB): the most it may fetch
/// from its source over its life, by identify and every extraction. Twice
/// [`MAX_EMBEDDED_IMAGE_BYTES`], so the largest extraction fits beside identify's scattered header
/// reads (well under a megabyte on every camera in the inventory) with room for blocks read again.
/// A caller's own budget goes below it: previews are for browsing, and a file whose previews
/// need more is better developed.
pub const MAX_EMBEDDED_READ_BUDGET: u64 = 128 * 1024 * 1024;

/// Above this many pixels a per-pixel pass that is not a rendering pass moves from a serial loop to
/// the shared Rayon pool: the RAW development passes (the camera-matrix conversion and the DNG
/// corrections), a spatial operation's global-estimate reduction, the analysis reducer and the
/// overlays. Rendering passes read their own thresholds from core rendering.
pub const PARALLEL_PIXELS: u64 = 1_000_000;
