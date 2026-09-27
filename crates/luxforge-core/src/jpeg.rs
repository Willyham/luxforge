//! The one JPEG codec in the shipped crates: libjpeg-turbo as bundled by `mozjpeg-sys`, through the
//! `mozjpeg` crate's safe API, reading originals ([`Decoder`]) and writing exports ([`encode`]).
//! Nothing else names `mozjpeg`, which `cargo xtask check-repository` enforces, so the safety
//! around the C library lives here once and the call sites keep only their policy.
//!
//! Contract:
//! - libjpeg reports an error by unwinding (`mozjpeg-sys` builds it with `-fexceptions` for that).
//!   Every call into it runs under `catch_unwind`, so a failure is an [`Error`], never an abort. A
//!   session a failure interrupted is destroyed at once and never called again, since libjpeg
//!   leaves the object undefined after an error; dropping a session only destroys it, which cannot
//!   fail.
//! - Decoding treats every libjpeg warning as an error. libjpeg warns and carries on when the data
//!   is corrupt or truncated ("Premature end of JPEG file", a bad Huffman code, data cut short by a
//!   marker), filling what it could not decode with grey; an original must never import as silently
//!   wrong pixels. The decoder's own error manager unwinds on the first warning, as TurboJPEG's
//!   stop-on-warning mode does. libjpeg's message text is not readable through the safe API (it
//!   sits behind the error manager's raw pointer), so a decode failure says which kind it was, not
//!   libjpeg's words.
//! - [`Decoder::new`] reads only the header: libjpeg's component tables and the saved APP2
//!   segments, which the encoded bytes bound. It checks the declared dimensions against the
//!   caller's [`Limits`] and the component count before libjpeg allocates anything that scales with
//!   the image. Pixels arrive as RGBA8 rows written by libjpeg straight into the caller's slice,
//!   greyscale expanded to grey RGB and alpha 255.
//! - The ICC profile is reassembled here from its APP2 `ICC_PROFILE` chunks, numbered from 1 as the
//!   ICC specification (ICC.1, Annex B.4) requires; a chunk sequence that does not follow it is an
//!   unsupported profile, not a missing one.
//! - [`encode`] writes libjpeg's fastest baseline profile (one interleaved scan, standard Huffman
//!   tables) at the caller's quality and chroma sampling, with the caller's APPn segments after the
//!   JFIF header, streaming into the writer through libjpeg's buffer of at most 64 KiB. It uses
//!   `mozjpeg`'s default error manager, whose fatal errors carry libjpeg's message.

use crate::Error;
use mozjpeg::{ColorSpace, Compress, Decompress, Marker, decompress::DecompressStarted};
use mozjpeg_sys::{jpeg_common_struct, jpeg_error_mgr};
use std::{
    any::Any,
    cell::RefCell,
    io::{self, Write},
    os::raw::c_int,
    panic::{self, AssertUnwindSafe},
    ptr,
};

/// Rows handed to libjpeg per encode call: its largest MCU height, so a call ends on whole MCU rows
/// and the caller's step runs between calls without copying the frame.
pub(crate) const STRIP_ROWS: usize = 16;

/// The most one APPn segment carries after its length field.
pub(crate) const MAX_SEGMENT_PAYLOAD: usize = 65533;

const ICC_HEADER: &[u8] = b"ICC_PROFILE\0";

/// The largest ICC profile reassembled. `crate::profile::check` parses at most this much, so a
/// larger profile would be refused there too; stopping here bounds the copy.
const MAX_ICC_BYTES: usize = 1024 * 1024;

/// Runs one call into libjpeg, catching the unwind by which it reports an error.
fn guarded<T>(call: impl FnOnce() -> T) -> Result<T, Box<dyn Any + Send>> {
    panic::catch_unwind(AssertUnwindSafe(call))
}

/// What a decode's error manager unwinds with.
enum Stop {
    /// libjpeg's `error_exit`: data it cannot decode or does not support.
    Fatal,
    /// libjpeg's `emit_message` at warning level: corrupt or truncated data it would have papered
    /// over.
    Warning,
}

extern "C-unwind" fn stop_on_error(_: &mut jpeg_common_struct) {
    panic::resume_unwind(Box::new(Stop::Fatal));
}

/// Warnings (level -1) stop the decode; trace messages (level 0 and above) are ignored.
extern "C-unwind" fn stop_on_warning(_: &mut jpeg_common_struct, level: c_int) {
    if level < 0 {
        panic::resume_unwind(Box::new(Stop::Warning));
    }
}

extern "C-unwind" fn silent(_: &mut jpeg_common_struct) {}

extern "C-unwind" fn no_text(_: &mut jpeg_common_struct, _: &[u8; 80]) {}

/// The decoder's error manager: every error and warning unwinds, nothing is printed. libjpeg calls
/// only `error_exit`, `emit_message` and `reset_error_mgr` itself; the message tables are read
/// only by the formatting functions replaced here, so they stay empty.
fn error_manager() -> jpeg_error_mgr {
    jpeg_error_mgr {
        error_exit: Some(stop_on_error),
        emit_message: Some(stop_on_warning),
        output_message: Some(silent),
        format_message: Some(no_text),
        reset_error_mgr: Some(silent),
        msg_code: 0,
        msg_parm: Default::default(),
        trace_level: 0,
        num_warnings: 0,
        jpeg_message_table: ptr::null(),
        last_jpeg_message: 0,
        addon_message_table: ptr::null(),
        first_addon_message: 0,
        last_addon_message: 0,
    }
}

/// The error a decode's unwind stands for.
fn decode_failure(payload: Box<dyn Any + Send>) -> Error {
    match payload.downcast_ref::<Stop>() {
        Some(Stop::Warning) => Error::decode("corrupt or truncated JPEG data"),
        Some(Stop::Fatal) => Error::decode("JPEG data libjpeg cannot decode"),
        // A panic in the bindings themselves, not libjpeg's.
        None => Error::internal(format!("jpeg decode: {}", panic_message(&payload))),
    }
}

fn panic_message(payload: &Box<dyn Any + Send>) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("libjpeg failed")
}

/// The largest image a decode accepts, checked against the header before any pixel allocation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) max_side: u32,
    pub(crate) max_pixels: u64,
}

/// Where a [`Decoder`]'s libjpeg session is.
enum Session<'a> {
    /// The header is read; nothing that scales with the image is allocated yet.
    Header(Decompress<&'a [u8]>),
    /// Decompressing to RGBA.
    Started(DecompressStarted<&'a [u8]>),
    /// Finished, or destroyed after a failed call: libjpeg is not called again.
    Ended,
}

/// One JPEG decode: the header read and checked, then RGBA rows on request.
pub(crate) struct Decoder<'a> {
    session: Session<'a>,
    width: u32,
    height: u32,
    components: u8,
    icc: Option<Vec<u8>>,
    rows_read: u32,
}

impl<'a> Decoder<'a> {
    /// Read the header and check it against `limits` and the supported colour spaces (greyscale,
    /// and three-component YCbCr or RGB), and reassemble the ICC profile. Decompression starts
    /// with the first [`Self::read_rows`], so a caller can refuse the profile first; a progressive
    /// frame is decoded into libjpeg's coefficient buffer then.
    pub(crate) fn new(bytes: &'a [u8], limits: Limits) -> Result<Self, Error> {
        let header = guarded(|| {
            Decompress::builder()
                .with_err(error_manager())
                .with_markers(&[Marker::APP(2)])
                .from_mem(bytes)
        })
        .map_err(decode_failure)?
        .map_err(|error| Error::decode(format!("JPEG header: {error}")))?;
        let (width, height) = header.size();
        let (width, height) = (width as u32, height as u32);
        if width == 0
            || height == 0
            || width > limits.max_side
            || height > limits.max_side
            || u64::from(width) * u64::from(height) > limits.max_pixels
        {
            return Err(Error::resource_limit("dimensions"));
        }
        let components = match (header.components().len(), header.color_space()) {
            (1, ColorSpace::JCS_GRAYSCALE) => 1,
            (3, ColorSpace::JCS_YCbCr | ColorSpace::JCS_RGB) => 3,
            _ => return Err(Error::unsupported_color("only RGB/greyscale")),
        };
        let icc = icc_profile(header.markers().map(|marker| marker.data))?;
        Ok(Self {
            session: Session::Header(header),
            width,
            height,
            components,
            icc,
            rows_read: 0,
        })
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    /// 1 for greyscale, 3 for colour.
    pub(crate) fn components(&self) -> u8 {
        self.components
    }

    /// The reassembled ICC profile, when the file carries one.
    pub(crate) fn icc_profile(&self) -> Option<&[u8]> {
        self.icc.as_deref()
    }

    /// Decode the next whole rows into `rows`, RGBA8, `width * 4` bytes each.
    pub(crate) fn read_rows(&mut self, rows: &mut [u8]) -> Result<(), Error> {
        let stride = self.width as usize * 4;
        let count = rows.len() / stride;
        if !rows.len().is_multiple_of(stride) || count > (self.height - self.rows_read) as usize {
            return Err(Error::internal(
                "jpeg decode: rows requested beyond the image or not whole rows",
            ));
        }
        let result = self.read_into(rows);
        match result {
            Ok(()) => self.rows_read += count as u32,
            Err(_) => self.session = Session::Ended,
        }
        result
    }

    fn read_into(&mut self, rows: &mut [u8]) -> Result<(), Error> {
        if let Session::Header(_) = self.session {
            let Session::Header(header) = std::mem::replace(&mut self.session, Session::Ended)
            else {
                unreachable!("matched above");
            };
            let started = guarded(move || header.rgba())
                .map_err(decode_failure)?
                .map_err(|error| Error::decode(format!("JPEG decode: {error}")))?;
            if (started.width(), started.height()) != (self.width as usize, self.height as usize) {
                return Err(Error::internal(
                    "jpeg decode: output size differs from the header",
                ));
            }
            self.session = Session::Started(started);
        }
        let Session::Started(started) = &mut self.session else {
            return Err(Error::internal("jpeg decode: the session has ended"));
        };
        match guarded(|| started.read_scanlines_into::<u8>(rows)) {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(Error::decode(format!("JPEG scan data: {error}"))),
            Err(payload) => Err(decode_failure(payload)),
        }
    }

    /// Read the rest of the file to EOI, so a warning about data after the last row is an error
    /// too, and end the session. Every row must have been read.
    pub(crate) fn finish(mut self) -> Result<(), Error> {
        if self.rows_read != self.height {
            return Err(Error::internal("jpeg decode: finished before the last row"));
        }
        let Session::Started(started) = std::mem::replace(&mut self.session, Session::Ended) else {
            return Err(Error::internal("jpeg decode: the session has ended"));
        };
        guarded(move || started.finish())
            .map_err(decode_failure)?
            .map_err(|error| Error::decode(format!("JPEG end: {error}")))
    }
}

/// The ICC profile carried by `segments` (APP2 payloads in file order): the `ICC_PROFILE` chunks,
/// each `ICC_PROFILE\0`, its sequence number from 1, the chunk count and its part, concatenated in
/// sequence order. `None` without chunks. Every chunk must declare the same count, which must equal
/// the number of chunks, with each number from 1 to that count exactly once.
fn icc_profile<'s>(segments: impl Iterator<Item = &'s [u8]>) -> Result<Option<Vec<u8>>, Error> {
    let unreadable = |why: &str| Error::unsupported_profile(format!("unreadable ICC: {why}"));
    let mut chunks: [Option<&[u8]>; 256] = [None; 256];
    let mut count = None;
    let mut found = 0_usize;
    let mut bytes = 0_usize;
    for segment in segments {
        let Some(chunk) = segment.strip_prefix(ICC_HEADER) else {
            continue;
        };
        let [sequence, total, data @ ..] = chunk else {
            return Err(unreadable("chunk header"));
        };
        if *count.get_or_insert(*total) != *total {
            return Err(unreadable("chunk counts differ"));
        }
        if *sequence == 0 || sequence > total {
            return Err(unreadable("chunk number out of range"));
        }
        let slot = &mut chunks[usize::from(*sequence)];
        if slot.is_some() {
            return Err(unreadable("repeated chunk number"));
        }
        bytes += data.len();
        if bytes > MAX_ICC_BYTES {
            return Err(Error::unsupported_profile("ICC profile larger than 1 MiB"));
        }
        *slot = Some(data);
        found += 1;
    }
    let Some(count) = count else {
        return Ok(None);
    };
    if found != usize::from(count) {
        return Err(unreadable("missing chunk"));
    }
    let mut profile = Vec::with_capacity(bytes);
    for chunk in &chunks[1..=usize::from(count)] {
        profile.extend_from_slice(chunk.expect("every numbered chunk was found"));
    }
    Ok(Some(profile))
}

/// How [`encode`] compresses: libjpeg's quality from 1 to 100, and the chroma sampling as the
/// pixels one chroma sample covers horizontally and vertically, `(1, 1)` for 4:4:4.
pub(crate) struct Settings<'a> {
    pub(crate) quality: u8,
    pub(crate) chroma: (u8, u8),
    /// APPn segments written after libjpeg's JFIF header, in order: `(n, payload)`, each payload
    /// at most [`MAX_SEGMENT_PAYLOAD`] bytes.
    pub(crate) segments: &'a [(u8, &'a [u8])],
}

/// Passes writes through and keeps the first I/O error, which libjpeg itself only reports as its
/// own fatal write error, so the encode can answer with the file system's reason.
struct RecordingWriter<'a, W> {
    inner: W,
    error: &'a RefCell<Option<io::Error>>,
}

impl<W> RecordingWriter<'_, W> {
    fn record<T>(&self, result: io::Result<T>) -> io::Result<T> {
        result.map_err(|error| {
            let kind = error.kind();
            if kind != io::ErrorKind::Interrupted {
                self.error.borrow_mut().get_or_insert(error);
            }
            io::Error::from(kind)
        })
    }
}

impl<W: Write> Write for RecordingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let result = self.inner.write(buf);
        self.record(result)
    }

    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        self.record(result)
    }
}

/// Encode the `width` × `height` RGBA8 `rgba` (alpha ignored) as a baseline JPEG into `out`,
/// [`STRIP_ROWS`] rows per libjpeg call. `step` is called with the rows written so far before each
/// call and once after the last, before libjpeg finishes the file; an error from it abandons the
/// encode, discarding what libjpeg still buffers, and is returned as it is. A writer failure is a
/// `file-access` error with the writer's reason, a libjpeg error a `render` error with libjpeg's
/// message.
pub(crate) fn encode<W: Write>(
    out: W,
    width: u32,
    height: u32,
    rgba: &[u8],
    settings: &Settings<'_>,
    step: &mut dyn FnMut(usize) -> Result<(), Error>,
) -> Result<(), Error> {
    let stride = width as usize * 4;
    let rows = height as usize;
    if rgba.len() != stride * rows {
        return Err(Error::internal(
            "jpeg encode: the frame's pixels do not match its dimensions",
        ));
    }
    if settings
        .segments
        .iter()
        .any(|(_, payload)| payload.len() > MAX_SEGMENT_PAYLOAD)
    {
        return Err(Error::internal(
            "jpeg encode: a segment does not fit one APPn marker",
        ));
    }
    let io_error = RefCell::<Option<io::Error>>::new(None);
    let failure = |payload: Box<dyn Any + Send>| {
        if let Some(error) = io_error.borrow_mut().take() {
            return Error::file_access(error.to_string());
        }
        Error::render(format!("jpeg encode: {}", panic_message(&payload)))
    };
    let refused = |error: io::Error| Error::render(format!("jpeg encode: {error}"));
    let writer = RecordingWriter {
        inner: out,
        error: &io_error,
    };

    // libjpeg's fastest profile is plain libjpeg-turbo: baseline, one interleaved scan, standard
    // Huffman tables and no trellis quantization.
    let mut started = guarded(|| {
        let mut compress = Compress::new(ColorSpace::JCS_EXT_RGBA);
        compress.set_fastest_defaults();
        compress.set_size(width as usize, rows);
        compress.set_quality(f32::from(settings.quality));
        let (h, v) = settings.chroma;
        compress.set_chroma_sampling_pixel_sizes((h, v), (h, v));
        compress.start_compress(writer)
    })
    .map_err(&failure)?
    .map_err(refused)?;
    // `start_compress` has written SOI and the JFIF APP0; the other segments follow it in order.
    guarded(|| {
        for (n, payload) in settings.segments {
            started.write_marker(Marker::APP(*n), payload);
        }
    })
    .map_err(&failure)?;

    for (index, strip) in rgba.chunks(STRIP_ROWS * stride).enumerate() {
        if let Err(error) = step(index * STRIP_ROWS) {
            // Discards what libjpeg still buffers; the compressor is destroyed when dropped.
            drop(started.abort());
            return Err(error);
        }
        guarded(|| started.write_scanlines(strip))
            .map_err(&failure)?
            .map_err(refused)?;
    }
    if let Err(error) = step(rows) {
        drop(started.abort());
        return Err(error);
    }
    guarded(|| started.finish())
        .map_err(&failure)?
        .map_err(refused)?;
    Ok(())
}

#[cfg(test)]
mod tests;
