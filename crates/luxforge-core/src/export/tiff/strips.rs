//! A TIFF export's strips: RGB rows taken from opaque RGBA bands as they arrive, each strip written
//! as soon as its last row is, uncompressed or as one zlib (Deflate) stream of horizontally
//! differenced rows. It holds one RGB row, the Deflate state and its 64 KiB output buffer, and the
//! strip tables (8 bytes a strip), whatever the bands' sizes, never a band or a strip.
//!
//! This file names only the crate root's `Error` and `tiles` and its own dependencies, so a test
//! binary of its own compiles it with a counting allocator (`tests/tiff_strips`).

use crate::{
    Error,
    tiles::{Depth, PixelRows},
};
use flate2::{Compress, Compression, FlushCompress, Status};
use std::io::Write;

/// The uncompressed bytes a strip aims at: large enough that Deflate's 32 KiB window and a strip's
/// zlib header and checksum cost nothing measurable, small enough for a reader to decode a region
/// without much more than it asked for.
pub(crate) const STRIP_BYTES: usize = 256 * 1024;

/// zlib's default level: most of level 9's gain at a fraction of its time.
const DEFLATE_LEVEL: u32 = 6;

/// The compressed bytes written at a time.
const DEFLATE_CHUNK: usize = 64 * 1024;

/// Bytes a pixel of a TIFF export: three samples of `depth`.
pub(crate) const fn pixel_bytes(depth: Depth) -> usize {
    3 * depth.bytes()
}

/// The rows a strip holds for a `width` × `depth` image: [`STRIP_BYTES`] of them, at least one.
pub(crate) fn rows_per_strip(width: u32, depth: Depth) -> u32 {
    let row = width as usize * pixel_bytes(depth);
    u32::try_from((STRIP_BYTES / row.max(1)).max(1)).unwrap_or(u32::MAX)
}

struct Deflate {
    stream: Compress,
    chunk: Box<[u8]>,
}

pub(crate) struct Strips {
    width: u32,
    height: u32,
    depth: Depth,
    rows_per_strip: u32,
    deflate: Option<Deflate>,
    /// One row as written: RGB, differenced when compressing, 16-bit samples little-endian.
    row: Vec<u8>,
    rows: u32,
    /// Where the next byte goes, from the TIFF's start.
    at: u64,
    strip_start: u64,
    offsets: Vec<u32>,
    byte_counts: Vec<u32>,
}

impl Strips {
    /// Strips of a `width` × `height` image of `depth` samples, the first written at `at`. The
    /// caller keeps the file within 4 GiB, so every offset and count fits a LONG.
    pub(crate) fn new(width: u32, height: u32, depth: Depth, compress: bool, at: u64) -> Self {
        let rows_per_strip = rows_per_strip(width, depth).min(height.max(1));
        Self {
            width,
            height,
            depth,
            rows_per_strip,
            deflate: compress.then(|| Deflate {
                stream: Compress::new(Compression::new(DEFLATE_LEVEL), true),
                chunk: vec![0; DEFLATE_CHUNK].into_boxed_slice(),
            }),
            row: Vec::with_capacity(width as usize * pixel_bytes(depth)),
            rows: 0,
            at,
            strip_start: at,
            offsets: Vec::with_capacity(height.div_ceil(rows_per_strip) as usize),
            byte_counts: Vec::with_capacity(height.div_ceil(rows_per_strip) as usize),
        }
    }

    pub(crate) fn rows_per_strip(&self) -> u32 {
        self.rows_per_strip
    }

    /// Rows written so far.
    pub(crate) fn rows(&self) -> u32 {
        self.rows
    }

    /// Where the next byte goes, from the TIFF's start.
    pub(crate) fn at(&self) -> u64 {
        self.at
    }

    /// Each written strip's offset and byte count.
    pub(crate) fn tables(&self) -> (&[u32], &[u32]) {
        (&self.offsets, &self.byte_counts)
    }

    /// Write `rows` rows of `pixels` (RGBA, alpha dropped) to `out`, the next of the image, ending
    /// each strip as its last row is written. `before_strip` runs before each strip's first row
    /// with the rows written so far; its error stops the writing and is returned as it is. Pixels
    /// that are not `rows` whole rows of the image's depth, or rows past its last, are refused as
    /// `internal` before anything is written; a failure to write is `file-access`.
    pub(crate) fn write_rows<W: Write>(
        &mut self,
        out: &mut W,
        rows: u32,
        pixels: PixelRows<'_>,
        before_strip: &mut dyn FnMut(u32) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let stride = self.width as usize * 4;
        if pixels.depth() != self.depth {
            return Err(Error::internal(
                "a band's bit depth is not the TIFF export's",
            ));
        }
        if pixels.len() != rows as usize * stride {
            return Err(Error::internal(
                "a band's pixels are not the rows it declares",
            ));
        }
        if rows > self.height - self.rows {
            return Err(Error::internal(
                "a band runs past the TIFF export's last row",
            ));
        }
        for index in 0..rows as usize {
            if self.rows.is_multiple_of(self.rows_per_strip) {
                before_strip(self.rows)?;
                self.strip_start = self.at;
                if let Some(deflate) = &mut self.deflate {
                    deflate.stream.reset();
                }
            }
            let samples = index * stride..(index + 1) * stride;
            let predict = self.deflate.is_some();
            match pixels {
                PixelRows::Eight(rgba) => rgb8(&mut self.row, &rgba[samples], predict),
                PixelRows::Sixteen(rgba) => rgb16(&mut self.row, &rgba[samples], predict),
            }
            match &mut self.deflate {
                Some(deflate) => {
                    self.at += deflate.feed(out, &self.row, FlushCompress::None)?;
                }
                None => {
                    out.write_all(&self.row).map_err(written)?;
                    self.at += self.row.len() as u64;
                }
            }
            self.rows += 1;
            if self.rows.is_multiple_of(self.rows_per_strip) || self.rows == self.height {
                if let Some(deflate) = &mut self.deflate {
                    self.at += deflate.feed(out, &[], FlushCompress::Finish)?;
                }
                self.offsets.push(self.strip_start as u32);
                self.byte_counts.push((self.at - self.strip_start) as u32);
            }
        }
        Ok(())
    }
}

impl Deflate {
    /// Compress `input` into the strip's stream, writing what Deflate produces to `out`; with
    /// `Finish`, end the stream. Returns the bytes written.
    fn feed<W: Write>(
        &mut self,
        out: &mut W,
        mut input: &[u8],
        flush: FlushCompress,
    ) -> Result<u64, Error> {
        let finishing = matches!(flush, FlushCompress::Finish);
        let mut written_bytes = 0;
        loop {
            let (read_before, made_before) = (self.stream.total_in(), self.stream.total_out());
            let status = self
                .stream
                .compress(input, &mut self.chunk, flush)
                .map_err(|error| Error::internal(format!("Deflate failed: {error}")))?;
            let read = (self.stream.total_in() - read_before) as usize;
            let made = (self.stream.total_out() - made_before) as usize;
            input = &input[read..];
            out.write_all(&self.chunk[..made]).map_err(written)?;
            written_bytes += made as u64;
            let ended = if finishing {
                status == Status::StreamEnd
            } else {
                // Everything taken and nothing more waiting to come out.
                input.is_empty() && made < self.chunk.len()
            };
            if ended {
                return Ok(written_bytes);
            }
            if read == 0 && made == 0 {
                if !finishing && input.is_empty() {
                    // The last chunk was exactly full and nothing more was waiting.
                    return Ok(written_bytes);
                }
                return Err(Error::internal("Deflate made no progress"));
            }
        }
    }
}

fn written(error: std::io::Error) -> Error {
    Error::file_access(format!("writing the TIFF failed: {error}"))
}

/// One row of 8-bit RGBA as RGB, each sample less the one a pixel to its left when `predict`.
fn rgb8(row: &mut Vec<u8>, rgba: &[u8], predict: bool) {
    row.resize(rgba.len() / 4 * 3, 0);
    let mut left = [0u8; 3];
    for (rgb, pixel) in row.chunks_exact_mut(3).zip(rgba.chunks_exact(4)) {
        let samples = [pixel[0], pixel[1], pixel[2]];
        if predict {
            for channel in 0..3 {
                rgb[channel] = samples[channel].wrapping_sub(left[channel]);
            }
            left = samples;
        } else {
            rgb.copy_from_slice(&samples);
        }
    }
}

/// [`rgb8`] for 16-bit samples, written little-endian as the file's byte order says.
fn rgb16(row: &mut Vec<u8>, rgba: &[u16], predict: bool) {
    row.resize(rgba.len() / 4 * 6, 0);
    let mut left = [0u16; 3];
    for (rgb, pixel) in row.chunks_exact_mut(6).zip(rgba.chunks_exact(4)) {
        for channel in 0..3 {
            let sample = if predict {
                pixel[channel].wrapping_sub(left[channel])
            } else {
                pixel[channel]
            };
            rgb[channel * 2..channel * 2 + 2].copy_from_slice(&sample.to_le_bytes());
        }
        left = [pixel[0], pixel[1], pixel[2]];
    }
}
