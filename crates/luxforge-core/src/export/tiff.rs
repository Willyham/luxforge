//! An RGB TIFF of 8 or 16 bits a sample, written by strips as an export's bands arrive
//! (`docs/design/export-settings.md`, encoding).
//!
//! Contract:
//! - [`TiffWriter::start`] writes the 8-byte little-endian header into `out`, from its current
//!   position, which becomes the TIFF's offset 0. [`TiffWriter::write_rows`] takes the image's rows
//!   in order from the top in bands of any size, as opaque RGBA [`PixelRows`] of the options'
//!   [`Depth`] (8-bit codes, or 16-bit codes in native order), drops the alpha sample, and writes
//!   each strip of [`strips::STRIP_BYTES`] uncompressed bytes as soon as its last row has arrived.
//!   [`TiffWriter::finish`] writes the directories after the strips, points the header at IFD0 and
//!   hands `out` back, positioned at the file's end.
//! - It holds one RGB row, the Deflate state and its 64 KiB output buffer, the strip tables (8
//!   bytes a strip), the ICC profile and the metadata, never a band or a strip: the caller's band
//!   is read in place.
//! - [`TiffOptions::compress`] is lossless Deflate (TIFF compression 8, zlib level 6) over rows
//!   differenced horizontally (Predictor 2), each strip one zlib stream; without it the strips are
//!   the samples as they are. Either way the pixels decode to exactly the samples given.
//! - IFD0 carries the image's structure, its density (XResolution and YResolution at
//!   `pixels_per_inch` with ResolutionUnit inch, or 1/1 with no unit when there is none) and the ICC
//!   profile in tag 34675. With `metadata`, IFD0 adds its descriptive fields and the structural
//!   fields the EXIF payload carries (Orientation 1, Software), its capture fields go in an Exif
//!   IFD (with ExifVersion, ColorSpace sRGB and the pixel dimensions) and its GPS fields in a GPS
//!   IFD; `None` writes no EXIF field and no other IFD. The metadata is written as given: choosing
//!   a level's fields is the caller's.
//! - A classic TIFF's offsets are 32 bits, so an image whose file could pass 4 GiB is refused at
//!   [`TiffWriter::start`], before anything is written, with the code [`TIFF_TOO_LARGE`]; that is
//!   a 16-bit image over about 715 MP or an 8-bit one over about 1.4 GP
//!   ([`tiff_worst_case_bytes`]).
//! - `progress` receives the fraction of rows written, at most about once per 1%, ending at 1;
//!   `cancel` is checked before every strip, and its error stops the writing and is returned as it
//!   is. A band that is not its rows, of another depth or past the last row, and a finish before
//!   the last row, are refused as `internal`; a failure of `out` is `file-access`. However the
//!   writing stops, nothing more is written.

mod strips;

use super::metadata::CaptureMetadata;
use crate::{
    Error,
    tiles::{Depth, PixelRows},
};
use serde_json::json;
use std::io::{Seek, SeekFrom, Write};
use strips::Strips;

/// The code a TIFF too large for 32-bit offsets is refused with.
pub(crate) const TIFF_TOO_LARGE: &str = "tiff-too-large";

/// The largest file a classic TIFF's 32-bit offsets address.
const MAX_FILE_BYTES: u64 = u32::MAX as u64;

/// How a TIFF export is written.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TiffOptions {
    /// Bits a sample: 8 or 16.
    pub(crate) depth: Depth,
    /// Lossless Deflate with the horizontal predictor.
    pub(crate) compress: bool,
    /// The density written as XResolution and YResolution, in pixels per inch.
    pub(crate) pixels_per_inch: Option<u16>,
}

/// What IFD0 says about the image, for the directory writer (`metadata/write.rs`).
pub(crate) struct TiffImage<'a> {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) bits_per_sample: u16,
    pub(crate) compressed: bool,
    pub(crate) rows_per_strip: u32,
    pub(crate) strip_offsets: &'a [u32],
    pub(crate) strip_byte_counts: &'a [u32],
    pub(crate) pixels_per_inch: Option<u16>,
    pub(crate) icc: &'a [u8],
}

/// The most bytes a `width` × `height` TIFF of `depth` samples, with an ICC profile of `icc_bytes`
/// and any metadata, can take: the samples, Deflate's worst case over them (incompressible data
/// stored, a 5-byte header per block and each strip's zlib header and checksum, bounded together
/// by 1/1024 of the samples and 64 bytes a strip), the strip tables and the directories.
pub(crate) fn tiff_worst_case_bytes(
    width: u32,
    height: u32,
    depth: Depth,
    icc_bytes: usize,
) -> u64 {
    let samples = u64::from(width) * u64::from(height) * strips::pixel_bytes(depth) as u64;
    let strips = u64::from(height.div_ceil(strips::rows_per_strip(width, depth)));
    let deflate = samples / 1024 + strips * 64;
    // IFD0's entries and short values, the EXIF payload's bound and its own IFD headers.
    let directories = 2 * 64 * 1024 + strips * 8 + icc_bytes as u64;
    8 + samples + deflate + directories
}

/// Whether a `width` × `height` TIFF of `depth` samples with the export's sRGB profile always
/// fits a classic TIFF's 4 GiB, as [`TiffWriter::start`] requires: what `export.plan` refuses
/// `tiff-too-large` before anything renders.
pub(crate) fn tiff_fits(width: u32, height: u32, depth: Depth) -> bool {
    tiff_worst_case_bytes(width, height, depth, super::encode::srgb_profile().len())
        <= MAX_FILE_BYTES
}

/// The refusal of a TIFF whose file could pass 4 GiB, with [`TIFF_TOO_LARGE`] as `data.code`.
pub(crate) fn too_large(width: u32, height: u32, depth: Depth) -> Error {
    let bits = depth.bytes() * 8;
    Error::validation(format!(
        "a {width} × {height} {bits}-bit TIFF could pass the format's 4 GiB limit"
    ))
    .with_data(json!({
        "code": TIFF_TOO_LARGE,
        "width": width,
        "height": height,
        "bit_depth": bits,
        "limit_bytes": MAX_FILE_BYTES,
    }))
}

/// A TIFF being written (the module's contract).
pub(crate) struct TiffWriter<W: Write + Seek> {
    out: W,
    origin: u64,
    width: u32,
    height: u32,
    options: TiffOptions,
    icc: Vec<u8>,
    metadata: Option<CaptureMetadata>,
    strips: Strips,
    reported: f64,
}

impl<W: Write + Seek> TiffWriter<W> {
    /// Check the image fits a TIFF and write its header at `out`'s position.
    pub(crate) fn start(
        mut out: W,
        width: u32,
        height: u32,
        options: &TiffOptions,
        icc: &[u8],
        metadata: Option<&CaptureMetadata>,
    ) -> Result<Self, Error> {
        if width == 0 || height == 0 {
            return Err(Error::internal("a TIFF export needs at least one pixel"));
        }
        if tiff_worst_case_bytes(width, height, options.depth, icc.len()) > MAX_FILE_BYTES {
            return Err(too_large(width, height, options.depth));
        }
        let origin = out.stream_position().map_err(written)?;
        out.write_all(b"II*\0\0\0\0\0").map_err(written)?;
        Ok(Self {
            out,
            origin,
            width,
            height,
            options: *options,
            icc: icc.to_vec(),
            metadata: metadata.cloned(),
            strips: Strips::new(width, height, options.depth, options.compress, 8),
            reported: 0.0,
        })
    }

    /// Write the next `rows` rows of the image, held in `pixels` (the module's contract).
    pub(crate) fn write_rows(
        &mut self,
        rows: u32,
        pixels: PixelRows<'_>,
        progress: &mut dyn FnMut(f64),
        cancel: &dyn Fn() -> Result<(), Error>,
    ) -> Result<(), Error> {
        let height = self.height;
        let reported = &mut self.reported;
        let mut report = |done: u32| {
            let fraction = f64::from(done) / f64::from(height);
            if fraction > *reported && (fraction - *reported >= 0.01 || done == height) {
                *reported = fraction;
                progress(fraction);
            }
        };
        self.strips
            .write_rows(&mut self.out, rows, pixels, &mut |done| {
                cancel()?;
                report(done);
                Ok(())
            })?;
        report(self.strips.rows());
        Ok(())
    }

    /// Write the directories after the strips and point the header at them (the module's
    /// contract).
    pub(crate) fn finish(mut self) -> Result<W, Error> {
        if self.strips.rows() != self.height {
            return Err(Error::internal(
                "the TIFF export finished before the last row",
            ));
        }
        let mut at = self.strips.at();
        if at % 2 == 1 {
            // IFDs begin on a word boundary.
            self.out.write_all(&[0]).map_err(written)?;
            at += 1;
        }
        let (strip_offsets, strip_byte_counts) = self.strips.tables();
        let image = TiffImage {
            width: self.width,
            height: self.height,
            bits_per_sample: (self.options.depth.bytes() * 8) as u16,
            compressed: self.options.compress,
            rows_per_strip: self.strips.rows_per_strip(),
            strip_offsets,
            strip_byte_counts,
            pixels_per_inch: self.options.pixels_per_inch,
            icc: &self.icc,
        };
        // `start` refused any image whose file could pass 4 GiB, so `at` fits.
        let directories =
            CaptureMetadata::tiff_directories(self.metadata.as_ref(), &image, at as u32);
        self.out.write_all(&directories).map_err(written)?;
        let end = self.origin + at + directories.len() as u64;
        self.out
            .seek(SeekFrom::Start(self.origin + 4))
            .map_err(written)?;
        self.out
            .write_all(&(at as u32).to_le_bytes())
            .map_err(written)?;
        self.out.seek(SeekFrom::Start(end)).map_err(written)?;
        self.out.flush().map_err(written)?;
        Ok(self.out)
    }
}

fn written(error: std::io::Error) -> Error {
    Error::file_access(format!("writing the TIFF failed: {error}"))
}

// The WebP encoder's tests share its metadata.
#[cfg(test)]
pub(crate) mod tests;
