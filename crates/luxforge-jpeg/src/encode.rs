//! One baseline JPEG encode, streamed into the caller's writer a strip at a time: a session,
//! [`Encoder`], that takes the image's rows in order in bands of any size, and [`encode`], which
//! is that session given the whole frame as one band.

use crate::{
    JpegError, MAX_SEGMENT_PAYLOAD, STRIP_ROWS,
    decode::{guarded, panic_message},
    icc,
};
use mozjpeg::{ColorSpace, Compress, Marker, compress::CompressStarted};
use std::{
    any::Any,
    cell::RefCell,
    io::{self, Write},
    rc::Rc,
};

/// How [`encode`] compresses: libjpeg's quality from 1 to 100, and the chroma sampling as the
/// pixels one chroma sample covers horizontally and vertically, `(1, 1)` for 4:4:4.
pub struct Settings<'a> {
    pub quality: u8,
    pub chroma: (u8, u8),
    /// APPn segments written after libjpeg's JFIF header, in order: `(n, payload)`, each payload
    /// at most [`MAX_SEGMENT_PAYLOAD`] bytes.
    pub segments: &'a [(u8, &'a [u8])],
    /// The ICC profile, written after `segments` as its APP2 chunks numbered from 1, as many as it
    /// takes up to 255.
    pub icc: Option<&'a [u8]>,
}

/// Passes writes through and keeps the first I/O error, which libjpeg itself only reports as its
/// own fatal write error, so the encode can answer with the file system's reason. The session
/// shares the slot, because libjpeg owns the writer until the encode ends.
struct RecordingWriter<W> {
    inner: W,
    error: Rc<RefCell<Option<io::Error>>>,
}

impl<W> RecordingWriter<W> {
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

impl<W: Write> Write for RecordingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let result = self.inner.write(buf);
        self.record(result)
    }

    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        self.record(result)
    }
}

/// What a libjpeg call that unwound stands for: the writer's own error when a write failed,
/// otherwise libjpeg's message.
fn failure(io_error: &RefCell<Option<io::Error>>, payload: Box<dyn Any + Send>) -> JpegError {
    if let Some(error) = io_error.borrow_mut().take() {
        return JpegError::Write(error);
    }
    JpegError::Encode(panic_message(&payload).to_owned())
}

/// A call `mozjpeg` refused without reaching libjpeg's error manager.
fn refused(error: io::Error) -> JpegError {
    JpegError::Encode(error.to_string())
}

fn ended() -> JpegError {
    JpegError::Internal("jpeg encode: the session has ended".into())
}

/// One JPEG encode in progress, streaming into its writer: [`Self::start`] writes the header, then
/// [`Self::write_rows`] takes the image's rows from the top in bands of any number of whole rows,
/// and [`Self::finish`] writes the trailer, or [`Self::abort`] abandons the file, as dropping the
/// session does.
///
/// libjpeg is handed the rows in strips of [`STRIP_ROWS`] counted from the top of the image,
/// whatever the bands: each strip a band holds whole is read in place, and a strip a band leaves
/// unfinished is copied into the session, at most one strip, and completed from the next band. So
/// libjpeg receives the same calls, and writes the same bytes, however the rows arrive, and the
/// caller's step runs between the same strips as it does for a whole frame ([`encode`]).
pub struct Encoder<W> {
    /// The libjpeg session until it finishes, is abandoned, or fails, after which it is destroyed
    /// and never called again.
    started: Option<CompressStarted<RecordingWriter<W>>>,
    io_error: Rc<RefCell<Option<io::Error>>>,
    /// Bytes per row, four per pixel.
    stride: usize,
    height: usize,
    /// Rows handed to libjpeg: whole strips until the last.
    written: usize,
    /// Rows received but not yet handed to libjpeg, fewer than one strip: the end of a band whose
    /// strip the next band completes. Allocated at its first use.
    pending: Vec<u8>,
}

impl<W: Write> Encoder<W> {
    /// Start encoding a `width` × `height` RGBA8 image (alpha ignored) as a baseline JPEG into
    /// `out`: libjpeg writes SOI and its JFIF APP0, then the session writes `settings.segments`
    /// in order and the ICC profile's chunks, all streamed through libjpeg's buffer of at most 64
    /// KiB. A segment too large for one marker and an ICC profile that is empty or too large for
    /// 255 chunks are refused before anything is written; libjpeg refuses an empty image, or one
    /// past its 65,500-pixel side, as [`JpegError::Encode`] with its message.
    pub fn start(
        out: W,
        width: u32,
        height: u32,
        settings: &Settings<'_>,
    ) -> Result<Self, JpegError> {
        if settings
            .segments
            .iter()
            .any(|(_, payload)| payload.len() > MAX_SEGMENT_PAYLOAD)
        {
            return Err(JpegError::Internal(
                "jpeg encode: a segment does not fit one APPn marker".into(),
            ));
        }
        let icc_chunks = settings.icc.map(icc::chunk_count).transpose()?;
        let io_error = Rc::new(RefCell::new(None));
        let writer = RecordingWriter {
            inner: out,
            error: Rc::clone(&io_error),
        };
        // libjpeg's fastest profile is plain libjpeg-turbo: baseline, one interleaved scan,
        // standard Huffman tables and no trellis quantization.
        let mut started = guarded(|| {
            let mut compress = Compress::new(ColorSpace::JCS_EXT_RGBA);
            compress.set_fastest_defaults();
            compress.set_size(width as usize, height as usize);
            compress.set_quality(f32::from(settings.quality));
            let (h, v) = settings.chroma;
            compress.set_chroma_sampling_pixel_sizes((h, v), (h, v));
            compress.start_compress(writer)
        })
        .map_err(|payload| failure(&io_error, payload))?
        .map_err(refused)?;
        // `start_compress` has written SOI and the JFIF APP0; the other segments follow it in
        // order, then the ICC chunks.
        guarded(|| {
            for (n, payload) in settings.segments {
                started.write_marker(Marker::APP(*n), payload);
            }
            if let (Some(profile), Some(count)) = (settings.icc, icc_chunks) {
                let mut chunk = Vec::with_capacity(MAX_SEGMENT_PAYLOAD);
                icc::write_chunks(profile, count, &mut chunk, |payload| {
                    started.write_marker(Marker::APP(2), payload);
                });
            }
        })
        .map_err(|payload| failure(&io_error, payload))?;
        Ok(Self {
            started: Some(started),
            io_error,
            stride: width as usize * 4,
            height: height as usize,
            written: 0,
            pending: Vec::new(),
        })
    }

    /// Encode the image's next `rows` rows: `rgba`, RGBA8 (alpha ignored), `width * 4` bytes a
    /// row, directly below the rows already given. `step` is called with the rows handed to
    /// libjpeg so far before each strip goes to it and, once this band completes the image, once
    /// more after the last strip; an error from it abandons the encode, as [`Self::abort`] does,
    /// and is returned as it is. A band whose bytes are not its `rows` whole rows, a band past the
    /// image's last row, and any band once the session has ended are refused as
    /// [`JpegError::Internal`] before libjpeg sees them, and the session stays as it was. A writer
    /// failure is [`JpegError::Write`] with the writer's error and a libjpeg error
    /// [`JpegError::Encode`] with libjpeg's message; either ends the session.
    pub fn write_rows<E: From<JpegError>>(
        &mut self,
        rows: usize,
        rgba: &[u8],
        step: &mut dyn FnMut(usize) -> Result<(), E>,
    ) -> Result<(), E> {
        if self.started.is_none() {
            return Err(ended().into());
        }
        if rows.checked_mul(self.stride) != Some(rgba.len()) {
            return Err(JpegError::Internal(
                "jpeg encode: a band's pixels are not the rows it declares".into(),
            )
            .into());
        }
        let received = self.written + self.pending.len() / self.stride;
        if rows > self.height - received {
            return Err(
                JpegError::Internal("jpeg encode: rows past the end of the image".into()).into(),
            );
        }
        if rows == 0 {
            return Ok(());
        }
        let strip = STRIP_ROWS * self.stride;
        let completes = received + rows == self.height;
        let mut band = rgba;
        // The strip an earlier band left unfinished, completed from this one.
        if !self.pending.is_empty() {
            let (head, rest) = band.split_at(band.len().min(strip - self.pending.len()));
            self.pending.extend_from_slice(head);
            band = rest;
            if self.pending.len() == strip || (completes && band.is_empty()) {
                let pending = std::mem::take(&mut self.pending);
                let handed = self.strip(&pending, step);
                self.pending = pending;
                self.pending.clear();
                handed?;
            }
        }
        // Whole strips in place, and the image's last strip, which may be short, when this band
        // ends the image.
        while band.len() >= strip || (completes && !band.is_empty()) {
            let (now, rest) = band.split_at(band.len().min(strip));
            self.strip(now, step)?;
            band = rest;
        }
        // Rows short of a strip wait for the next band.
        if !band.is_empty() {
            self.pending.reserve_exact(strip);
            self.pending.extend_from_slice(band);
        }
        if completes && let Err(error) = step(self.height) {
            self.abandon();
            return Err(error);
        }
        Ok(())
    }

    /// Hand libjpeg one strip of whole rows, after the caller's step.
    fn strip<E: From<JpegError>>(
        &mut self,
        rows: &[u8],
        step: &mut dyn FnMut(usize) -> Result<(), E>,
    ) -> Result<(), E> {
        if let Err(error) = step(self.written) {
            self.abandon();
            return Err(error);
        }
        let Some(started) = self.started.as_mut() else {
            return Err(ended().into());
        };
        match guarded(|| started.write_scanlines(rows)) {
            Ok(Ok(())) => {
                self.written += rows.len() / self.stride;
                Ok(())
            }
            // A failed session is destroyed at once: libjpeg leaves it undefined.
            Ok(Err(error)) => {
                self.started = None;
                Err(refused(error).into())
            }
            Err(payload) => {
                self.started = None;
                Err(failure(&self.io_error, payload).into())
            }
        }
    }

    /// Discard what libjpeg still buffers and destroy the session; the writer keeps what libjpeg
    /// had already written to it and receives nothing more.
    fn abandon(&mut self) {
        if let Some(started) = self.started.take() {
            drop(started.abort());
        }
    }

    /// Finish the file: libjpeg writes what it still buffers and the trailer, and flushes the
    /// writer. Every row must have been given; a session missing rows, or one that has ended, is
    /// refused as [`JpegError::Internal`] and abandoned, so nothing more is written.
    pub fn finish(mut self) -> Result<(), JpegError> {
        let Some(started) = self.started.take() else {
            return Err(ended());
        };
        if self.written != self.height {
            drop(started.abort());
            return Err(JpegError::Internal(
                "jpeg encode: finished before the last row".into(),
            ));
        }
        guarded(move || started.finish())
            .map_err(|payload| failure(&self.io_error, payload))?
            .map(drop)
            .map_err(refused)
    }

    /// Abandon the encode as a step's error does: libjpeg's buffered bytes are discarded and the
    /// writer keeps only what was already written to it, an incomplete file whose owner removes it.
    pub fn abort(mut self) {
        self.abandon();
    }
}

/// Encode the `width` × `height` RGBA8 `rgba` (alpha ignored) as a baseline JPEG into `out`: an
/// [`Encoder`] given the whole frame as one band, so libjpeg reads it in place, [`STRIP_ROWS`]
/// rows per call. `step` is called with the rows written so far before each call and once after
/// the last, before libjpeg finishes the file; an error from it abandons the encode, discarding
/// what libjpeg still buffers, and is returned as it is. A writer failure is [`JpegError::Write`]
/// with the writer's error, a libjpeg error [`JpegError::Encode`] with libjpeg's message; a frame
/// that does not match its dimensions, a segment too large for one marker and an ICC profile that
/// is empty or too large for 255 chunks are refused before anything is written.
pub fn encode<W: Write, E: From<JpegError>>(
    out: W,
    width: u32,
    height: u32,
    rgba: &[u8],
    settings: &Settings<'_>,
    step: &mut dyn FnMut(usize) -> Result<(), E>,
) -> Result<(), E> {
    let rows = height as usize;
    if (width as usize * 4).checked_mul(rows) != Some(rgba.len()) {
        return Err(JpegError::Internal(
            "jpeg encode: the frame's pixels do not match its dimensions".into(),
        )
        .into());
    }
    let mut encoder = Encoder::start(out, width, height, settings)?;
    encoder.write_rows(rows, rgba, step)?;
    Ok(encoder.finish()?)
}
