//! Luxforge's one JPEG codec: libjpeg-turbo as bundled by `mozjpeg-sys`, reading originals and
//! previews whole ([`Decoder`], at full size or one of libjpeg's DCT [`Scale`]s) or one rectangle
//! at full size ([`RegionDecoder`]), and writing exports ([`encode`]), with the JPEG container
//! around them: the bounded marker walk ([`header`], [`segments`]) and the ICC profile's APP2
//! chunks both ways. The whole decode and the encode go through the `mozjpeg` crate's safe API;
//! the region decode needs libjpeg's `cinfo` for its cropped columns and skipped rows, which that
//! API does not expose, so it runs its own libjpeg session over `mozjpeg-sys` (`session.rs`), the
//! crate's one module of `unsafe` calls into the C library. No other crate names `mozjpeg`, and
//! this one depends on no workspace crate, which `cargo xtask check-repository` enforces, so the
//! safety around the C library lives here once and the caller keeps only its policy: the limits it
//! passes in, and what each [`JpegError`] means to it.
//!
//! Contract:
//! - libjpeg reports an error by unwinding (`mozjpeg-sys` builds it with `-fexceptions` for that).
//!   Every call into it runs under `catch_unwind`, so a failure is a [`JpegError`], never an
//!   abort. A session a failure interrupted is destroyed at once and never called again, since
//!   libjpeg leaves the object undefined after an error; dropping a session only destroys it,
//!   which cannot fail.
//! - Decoding refuses every libjpeg warning that stands for missing, corrupt or guessed image data
//!   and goes on past the harmless ones, as the one table in `warnings.rs` classifies each code;
//!   a code it does not list refuses. libjpeg warns and carries on when the data is corrupt or
//!   truncated, filling what it could not decode with grey; an original must never import as
//!   silently wrong pixels. Both decoders use the one error manager that applies it.
//! - [`Decoder::new`] and [`RegionDecoder::new`] walk the header's marker segments themselves
//!   before libjpeg reads anything, then read only libjpeg's header: its component tables and the
//!   saved APP2 segments, which the encoded bytes bound. They check the declared dimensions
//!   against the caller's [`Limits`] and the component count before libjpeg allocates anything
//!   that scales with the image. Pixels arrive as RGBA8 rows written by libjpeg into the caller's
//!   slice (a region's through one row of its window), greyscale expanded to grey RGB and alpha
//!   255.
//! - [`Decoder::set_scale`] decodes at 1/2, 1/4 or 1/8 of the frame on each side by libjpeg's
//!   reduced inverse DCT, and [`Scale::covering`] picks the most reducing scale whose output still
//!   covers a target size, so that a later resample only shrinks. The frame is checked against the
//!   [`Limits`] at its full size whatever the scale: libjpeg's row buffers follow the output, but a
//!   multi-scan frame's coefficients are held for the whole frame.
//! - [`RegionDecoder`] decodes one rectangle at full scale to exactly the bytes of the same
//!   rectangle cut from a whole decode, with the same settings. libjpeg crops a window of whole
//!   iMCU columns, three left and one right of the rectangle, since it takes a crop's edges for
//!   the frame's in its chroma upsampling and progressive block smoothing, and skips the rows above
//!   it; it stops after the rectangle's last row without reading to EOI, so a warning about data
//!   after what the rectangle needs is not seen. The crate allocates the caller's rectangle and one
//!   window row; libjpeg's own buffers follow the frame the [`Limits`] bound: a few dozen rows of
//!   the full width, allocated before the crop narrows the work, and a multi-scan frame's whole
//!   coefficient array (2 bytes a coefficient, at most 6 bytes a pixel for 4:4:4).
//! - The ICC profile is reassembled from its APP2 `ICC_PROFILE` chunks and written as them,
//!   numbered from 1 as the ICC specification (ICC.1, Annex B.4) requires; a chunk sequence that
//!   does not follow it is an unsupported profile, not a missing one.
//! - [`encode`] writes libjpeg's fastest baseline profile (one interleaved scan, standard Huffman
//!   tables) at the caller's quality and chroma sampling, with the caller's APPn segments and then
//!   the ICC profile after the JFIF header, streaming into the writer through libjpeg's buffer of
//!   at most 64 KiB. It uses `mozjpeg`'s default error manager, whose fatal errors carry libjpeg's
//!   message.

mod container;
mod decode;
mod encode;
mod icc;
mod region;
mod scale;
mod session;
mod warnings;

pub use container::{Header, Segment, Segments, header, segments};
pub use decode::Decoder;
pub use encode::{Settings, encode};
pub use region::{Region, RegionDecoder};
pub use scale::Scale;

use std::{fmt, io};

/// Rows handed to libjpeg per encode call: its largest MCU height, so a call ends on whole MCU rows
/// and the caller's step runs between calls without copying the frame.
pub const STRIP_ROWS: usize = 16;

/// The most one APPn segment carries after its length field.
pub const MAX_SEGMENT_PAYLOAD: usize = 65533;

/// The largest image a decode accepts, checked against the header before any pixel allocation.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_side: u32,
    pub max_pixels: u64,
}

/// Why a decode or an encode stopped.
#[derive(Debug)]
pub enum JpegError {
    /// Not a JPEG, or a container this crate cannot walk: the text says what was wrong.
    Malformed(String),
    /// A frame whose samples are not 8-bit, or whose SOF segment is too short to say.
    Precision,
    /// No baseline, extended-sequential or progressive Huffman frame before the first scan.
    FrameType,
    /// Dimensions of zero, or beyond the caller's [`Limits`].
    Dimensions,
    /// Neither greyscale nor three-component YCbCr or RGB.
    ColourSpace,
    /// ICC chunks that do not follow ICC.1 Annex B.4, or a profile past the 1 MiB this crate
    /// reassembles.
    Icc(String),
    /// A libjpeg warning the table in `warnings.rs` refuses: the decode would be missing, corrupt
    /// or guessed image data. Holds libjpeg's message code.
    Corrupt(i32),
    /// A libjpeg fatal error while decoding: data it cannot decode or does not support.
    Undecodable,
    /// libjpeg refused an encode; the text is its message.
    Encode(String),
    /// The encode's writer failed, with the writer's own error.
    Write(io::Error),
    /// A call this crate refuses before libjpeg sees it (rows past the image, a segment too large
    /// for one marker), or a panic in the bindings themselves.
    Internal(String),
}

impl fmt::Display for JpegError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(why) => write!(f, "{why}"),
            Self::Precision => write!(f, "JPEG precision"),
            Self::FrameType => write!(f, "JPEG frame type"),
            Self::Dimensions => write!(f, "dimensions"),
            Self::ColourSpace => write!(f, "only RGB/greyscale"),
            Self::Icc(why) => write!(f, "{why}"),
            Self::Corrupt(code) => match warnings::name(*code) {
                Some(name) => write!(f, "corrupt or truncated JPEG data (libjpeg {name})"),
                None => write!(f, "corrupt or truncated JPEG data (libjpeg warning {code})"),
            },
            Self::Undecodable => write!(f, "JPEG data libjpeg cannot decode"),
            Self::Encode(why) => write!(f, "jpeg encode: {why}"),
            Self::Write(error) => write!(f, "{error}"),
            Self::Internal(why) => write!(f, "{why}"),
        }
    }
}

impl std::error::Error for JpegError {}

#[cfg(test)]
mod tests;
