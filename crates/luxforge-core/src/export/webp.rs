//! A lossy WebP of an export's whole output frame, through the one WebP codec (`luxforge-webp`,
//! libwebp) (`docs/design/export-settings.md`, encoding).
//!
//! Contract:
//! - `encode_webp(width, height, rgba, options, icc, exif, progress, cancel)` encodes the borrowed
//!   opaque RGBA8 sRGB frame (its alpha is never read) at `options.quality`, always 4:2:0, into
//!   memory and returns the whole file. [`WebpOptions::optimise`] is libwebp's slowest, best
//!   method with its sharp RGB-to-YUV conversion. `icc` is written as the `ICCP` chunk when not
//!   empty and `exif` (a TIFF structure, as `CaptureMetadata::exif_payload` writes it) as the
//!   `EXIF` chunk, in a VP8X container assembled by libwebp's mux.
//! - An output over [`WEBP_MAX_SIDE`] pixels a side is refused before anything is encoded, as
//!   [`too_large`] refuses it, with the code [`WEBP_TOO_LARGE`].
//! - It never copies the frame. It holds libwebp's YUV 4:2:0 planes (1.5 bytes a pixel), the
//!   encoder's state and the bitstream, then the mux's file and the returned copy of it; an
//!   optimised encode's sharp conversion also holds its working planes (about 7 bytes a pixel)
//!   while it converts. [`webp_held_bytes`] bounds the whole beyond the frame.
//! - `progress` receives libwebp's progress as a fraction, rising to 1, at most once per 1%.
//!   `cancel` is checked before the encode and at each of libwebp's reports; its error stops the
//!   encode and is returned as it is. libwebp does not report during the conversion, so a cancel
//!   then is seen when it ends.
//! - libwebp's failures are errors, never an abort: a frame that is not `width × height` pixels is
//!   `internal`, and a failure of the encoder or the mux is `render` with libwebp's reason.

use crate::Error;
use serde_json::json;

/// The longest side a WebP output may have: 16,383 pixels, VP8's limit.
pub(crate) const WEBP_MAX_SIDE: u32 = luxforge_webp::MAX_SIDE;

/// The code an output over [`WEBP_MAX_SIDE`] is refused with.
pub(crate) const WEBP_TOO_LARGE: &str = "webp-too-large";

/// How a WebP export is encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WebpOptions {
    /// libwebp's quality, 1 to 100.
    pub(crate) quality: u8,
    /// The slowest, best method with sharp RGB-to-YUV conversion.
    pub(crate) optimise: bool,
}

/// Whether a `width` × `height` output can be written as WebP.
pub(crate) const fn webp_fits(width: u32, height: u32) -> bool {
    width <= WEBP_MAX_SIDE && height <= WEBP_MAX_SIDE
}

/// The refusal of an output over [`WEBP_MAX_SIDE`] a side, with [`WEBP_TOO_LARGE`] as `data.code`.
pub(crate) fn too_large(width: u32, height: u32) -> Error {
    Error::validation(format!(
        "WebP is limited to {WEBP_MAX_SIDE} pixels a side; this output is {width} × {height}"
    ))
    .with_data(json!({
        "code": WEBP_TOO_LARGE,
        "width": width,
        "height": height,
        "limit": WEBP_MAX_SIDE,
    }))
}

/// The most bytes an encode of a `width` × `height` frame holds beyond the frame itself: libwebp's
/// planes and state, the bitstream and the file, as measured by the codec's footprint tests (about
/// 2.3 bytes a pixel plain and 8.8 optimised on a photograph-like frame), with room for detailed
/// frames' larger bitstreams: 4 bytes a pixel plain, 11 optimised.
pub(crate) const fn webp_held_bytes(width: u32, height: u32, optimise: bool) -> u64 {
    let pixels = width as u64 * height as u64;
    pixels * if optimise { 11 } else { 4 }
}

/// Encode one output frame as WebP (the module's contract).
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_webp(
    width: u32,
    height: u32,
    rgba: &[u8],
    options: &WebpOptions,
    icc: &[u8],
    exif: Option<&[u8]>,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> Result<(), Error>,
) -> Result<Vec<u8>, Error> {
    if !webp_fits(width, height) {
        return Err(too_large(width, height));
    }
    cancel()?;
    let settings = luxforge_webp::Settings {
        quality: options.quality,
        optimise: options.optimise,
        icc: (!icc.is_empty()).then_some(icc),
        exif,
    };
    let mut stopped = None;
    let mut reported = 0;
    let encoded = luxforge_webp::encode(width, height, rgba, &settings, &mut |percent| {
        if let Err(error) = cancel() {
            stopped = Some(error);
            return false;
        }
        if percent > reported {
            reported = percent;
            progress(f64::from(percent) / 100.0);
        }
        true
    });
    encoded.map_err(|error| match error {
        luxforge_webp::Error::Stopped => {
            stopped.unwrap_or_else(|| Error::cancelled("export encode cancelled"))
        }
        luxforge_webp::Error::Invalid(reason) => {
            Error::internal(format!("the WebP encoder refused the frame: {reason}"))
        }
        error => Error::render(error.to_string()),
    })
}

#[cfg(test)]
mod tests;
