//! The panorama merge provider's library (`docs/design/panorama-merge.md`).
//!
//! - [`frames`] is a panorama's working pixels: each RAW or JPEG frame read once, developed (a
//!   RAW with its detected lens profile baked, a JPEG into linear sRGB/D65 through its colour
//!   profile) and streamed by bands into the job's spool and its analysis proxy.

// The panorama merge job reads its frames through this; until it does, only the tests here do.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod frames;
