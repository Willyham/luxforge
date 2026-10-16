//! Luxforge's one WebP codec: libwebp as vendored and built by `libwebp-sys`, writing lossy exports
//! ([`encode`]). No other crate names `libwebp`, and this one depends on no workspace crate, which
//! `cargo xtask check-repository` enforces, so the safety around the C library lives here once and
//! the caller keeps only its policy: the settings it passes in, the size it allows, and what each
//! [`Error`] means to it.
//!
//! Contract:
//! - [`encode`] writes a lossy (VP8) WebP of an opaque RGBA8 frame at a quality from 0 to 100,
//!   always 4:2:0, into memory, and returns the whole file: a VP8X container assembled by
//!   libwebp's mux with the ICC profile as its `ICCP` chunk and the EXIF payload (a TIFF structure,
//!   without JPEG's `Exif\0\0` header) as its `EXIF` chunk, when given; with neither, the simple
//!   `VP8 ` container. The alpha samples are never read.
//! - [`Settings::optimise`] is libwebp's slowest, best method (6) with its sharp RGB-to-YUV
//!   conversion; without it the encode is libwebp's default method (4) and its plain conversion.
//! - The frame is borrowed, never copied. libwebp's own picture is its YUV 4:2:0 planes, which it
//!   fills from the frame: 1.5 bytes a pixel. Its picture import converts straight into them, but
//!   its sharp conversion only from an ARGB copy of the frame (4 more bytes a pixel), so the
//!   optimised encode calls libwebp's `sharpyuv` library on the borrowed frame itself, exactly as
//!   libwebp would on that copy; the conversion holds its own fixed-point working planes, about 7
//!   bytes a pixel, until it returns. Then the encoder holds its per-macroblock state and the
//!   bitstream, and the mux one copy of the file.
//! - `progress` receives libwebp's own percentage, rising from 0 to 100, through its progress hook;
//!   returning `false` stops the encode at libwebp's next report with [`Error::Stopped`]. A panic
//!   in it is caught at the C boundary and resumed once libwebp has returned.
//! - A frame that is not `width × height × 4` bytes, a side of 0 or over [`MAX_SIDE`], or a
//!   quality over 100 is refused before libwebp is called.

mod encode;

pub use encode::encode;

use std::fmt;

/// The longest side a WebP image may have: libwebp's `WEBP_MAX_DIMENSION`, 16,383 pixels, which
/// VP8's 14-bit dimension fields bound.
pub const MAX_SIDE: u32 = libwebp_sys::WEBP_MAX_DIMENSION;

/// How [`encode`] writes one image.
#[derive(Clone, Copy, Debug, Default)]
pub struct Settings<'a> {
    /// libwebp's lossy quality, 0 to 100.
    pub quality: u8,
    /// The slowest, best method with sharp RGB-to-YUV conversion.
    pub optimise: bool,
    /// The ICC profile, written as the `ICCP` chunk.
    pub icc: Option<&'a [u8]>,
    /// The EXIF payload, a TIFF structure, written as the `EXIF` chunk.
    pub exif: Option<&'a [u8]>,
}

/// Why [`encode`] wrote nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The frame or settings are not ones the encoder takes: the reason.
    Invalid(&'static str),
    /// The progress callback asked the encode to stop.
    Stopped,
    /// libwebp's encoder failed: its error.
    Encode(&'static str),
    /// libwebp's mux failed to assemble the file: the step and its error.
    Mux(&'static str, &'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(reason) => write!(f, "libwebp refused the frame: {reason}"),
            Self::Stopped => f.write_str("the WebP encode was stopped"),
            Self::Encode(error) => write!(f, "libwebp failed to encode: {error}"),
            Self::Mux(step, error) => write!(f, "libwebp's mux failed to {step}: {error}"),
        }
    }
}

impl std::error::Error for Error {}
