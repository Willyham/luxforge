//! Baseline quality-90 JPEG with an embedded sRGB ICC profile.
//!
//! Contract (`docs/design/export.md#behavior`, step 3):
//! - `encode_jpeg(out, frame, exif, progress, cancel)` encodes the RGBA8 sRGB `frame` (alpha is
//!   ignored; it is always 255) at [`super::QUALITY`], writes the JFIF header, the optional EXIF
//!   APP1 payload (`exif` excludes the `Exif\0\0` header) and the ICC profile, and streams the
//!   output into `out` without building the whole file in memory. `progress` receives the encoded
//!   fraction in `0..=1`, at most about once per 1% of rows; `cancel` is checked about as often, and
//!   a cancelled encode returns its error and writes nothing more.
//! - `srgb_profile()` is the one embedded profile, which `crate::profile::check` accepts.

use crate::{Error, Raster};
use std::io::Write;

pub fn encode_jpeg<W: Write>(
    _out: W,
    _frame: &Raster,
    _exif: Option<&[u8]>,
    _progress: &mut dyn FnMut(f64),
    _cancel: &dyn Fn() -> Result<(), Error>,
) -> Result<(), Error> {
    unimplemented!("export encoder")
}

pub fn srgb_profile() -> &'static [u8] {
    unimplemented!("export sRGB profile")
}
