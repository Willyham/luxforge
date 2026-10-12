//! One JPEG encode, streamed into the caller's writer a strip at a time: a session, [`Encoder`],
//! that takes the image's rows in order in bands of any size, and [`encode`], which is that session
//! given the whole frame as one band. A baseline encode writes as the rows arrive; an optimised one
//! holds the image's coefficients and writes the file in the passes [`Encoder::finish_with`] runs.

use crate::{
    JpegError, MAX_SEGMENT_PAYLOAD, STRIP_ROWS,
    compress::{Passes, Payload, Session, Stopped, sampling},
    decode::panic_message,
    icc,
};
use std::{
    cell::RefCell,
    io::{self, Write},
    os::raw::c_int,
    rc::Rc,
};

/// How [`encode`] compresses: libjpeg's quality from 1 to 100, and the chroma sampling as the
/// pixels one chroma sample covers horizontally and vertically, `(1, 1)` for 4:4:4 and `(2, 2)`
/// for 4:2:0.
pub struct Settings<'a> {
    pub quality: u8,
    pub chroma: (u8, u8),
    /// Optimise the file's size at the same quality: progressive scans (mozjpeg's fixed script of
    /// nine), optimised Huffman tables and trellis quantisation, with the same standard (Annex K)
    /// quantisation tables as the baseline, so a quality means the same tables either way. The
    /// encode holds the image's coefficients, [`coefficient_bytes`], until the file is written.
    /// Off, the file is libjpeg-turbo's baseline: one interleaved scan and the standard Huffman
    /// tables, written as the rows arrive.
    pub optimise: bool,
    /// The JFIF header's density in pixels per inch, the same both ways, or `None` for libjpeg's
    /// default, which names no unit and a 1:1 pixel aspect ratio.
    pub pixels_per_inch: Option<u16>,
    /// APPn segments written after libjpeg's JFIF header, in order: `(n, payload)`, each payload
    /// at most [`MAX_SEGMENT_PAYLOAD`] bytes.
    pub segments: &'a [(u8, &'a [u8])],
    /// The ICC profile, written after `segments` as its APP2 chunks numbered from 1, as many as it
    /// takes up to 255.
    pub icc: Option<&'a [u8]>,
}

/// The bytes an optimised encode of a `width` × `height` image at `chroma` sampling (as
/// [`Settings::chroma`]) holds in its coefficient arrays from its start until the file is written:
/// mozjpeg keeps two whole-image arrays, the quantised coefficients and the unquantised ones its
/// trellis quantisation reads, each 2 bytes a coefficient, 64 coefficients a block, for every
/// component's blocks padded to whole MCUs. That is 12 bytes a pixel at 4:4:4 and 6 at 4:2:0, a
/// little more where the edges pad. A baseline encode holds none of it. Pure: it allocates nothing.
pub fn coefficient_bytes(width: u32, height: u32, chroma: (u8, u8)) -> u64 {
    /// 64 coefficients of 2 bytes (`JBLOCK`).
    const BLOCK: u64 = 64 * 2;
    /// The quantised and the unquantised arrays (`whole_image`, `whole_image_uq` in `jccoefct.c`).
    const ARRAYS: u64 = 2;
    let (luma, chroma) = sampling(chroma);
    let (max_h, max_v) = (luma.0 as u64, luma.1 as u64);
    let blocks = |(h, v): (i32, i32)| {
        let (h, v) = (h as u64, v as u64);
        // libjpeg's `width_in_blocks`, rounded up to the component's sampling as `jccoefct.c`
        // pads its arrays.
        let across = (u64::from(width) * h)
            .div_ceil(max_h * 8)
            .next_multiple_of(h);
        let down = (u64::from(height) * v)
            .div_ceil(max_v * 8)
            .next_multiple_of(v);
        across * down
    };
    ARRAYS * BLOCK * (blocks(luma) + 2 * blocks(chroma))
}

/// How far an encode has got, as its step sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    /// Rows handed to libjpeg so far.
    pub rows: usize,
    /// The libjpeg pass under way, from 0: the rows arrive during pass 0, and an optimised encode
    /// runs the rest when it finishes. It is `passes` once an optimised encode is complete.
    pub pass: u32,
    /// libjpeg's passes over the image: 1 for a baseline encode.
    pub passes: u32,
    /// The fraction of the whole encode done, from 0 to 1 and never decreasing: each pass an equal
    /// share, as libjpeg's progress monitor counts them, and within the first, the rows given.
    pub fraction: f64,
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
fn failure(io_error: &RefCell<Option<io::Error>>, payload: Payload) -> JpegError {
    if let Some(error) = io_error.borrow_mut().take() {
        return JpegError::Write(error);
    }
    JpegError::Encode(panic_message(&payload).to_owned())
}

/// A call refused without reaching libjpeg's error manager.
fn refused(error: io::Error) -> JpegError {
    JpegError::Encode(error.to_string())
}

fn ended() -> JpegError {
    JpegError::Internal("jpeg encode: the session has ended".into())
}

/// One JPEG encode in progress, streaming into its writer: [`Self::start`] writes the header, then
/// [`Self::write_rows`] takes the image's rows from the top in bands of any number of whole rows,
/// and [`Self::finish_with`] (or [`Self::finish`]) writes the rest of the file, or [`Self::abort`]
/// abandons the file, as dropping the session does.
///
/// libjpeg is handed the rows in strips of [`STRIP_ROWS`] counted from the top of the image,
/// whatever the bands: each strip a band holds whole is read in place, and a strip a band leaves
/// unfinished is copied into the session, at most one strip, and completed from the next band. So
/// libjpeg receives the same calls, and writes the same bytes, however the rows arrive, and the
/// caller's step runs between the same strips as it does for a whole frame ([`encode`]).
pub struct Encoder<W> {
    /// The libjpeg session until it finishes, is abandoned, or fails, after which it is destroyed
    /// and never called again.
    started: Option<Session<RecordingWriter<W>>>,
    io_error: Rc<RefCell<Option<io::Error>>>,
    /// libjpeg's passes over the image, 1 for a baseline encode.
    passes: u32,
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
    /// Start encoding a `width` × `height` RGBA8 image (alpha ignored) into `out`, baseline or
    /// optimised as `settings` say: libjpeg writes SOI and its JFIF APP0, then the session writes
    /// `settings.segments` in order and the ICC profile's chunks, all streamed through libjpeg's
    /// buffer of at most 64 KiB. An optimised encode allocates its coefficient arrays here
    /// ([`coefficient_bytes`]). A segment too large for one marker and an ICC profile that is
    /// empty or too large for 255 chunks are refused before anything is written; libjpeg refuses
    /// an empty image, or one past its 65,500-pixel side, as [`JpegError::Encode`] with its
    /// message.
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
        let (mut started, passes) = Session::start(writer, width, height, settings)
            .map_err(|payload| failure(&io_error, payload))?;
        // `start` has written SOI and the JFIF APP0; the other segments follow it in order, then
        // the ICC chunks.
        let mut header = || -> Result<(), Payload> {
            for (n, payload) in settings.segments {
                started.write_marker(0xe0 + c_int::from(*n), payload)?;
            }
            if let (Some(profile), Some(count)) = (settings.icc, icc_chunks) {
                let mut chunk = Vec::with_capacity(MAX_SEGMENT_PAYLOAD);
                let mut written = Ok(());
                icc::write_chunks(profile, count, &mut chunk, |payload| {
                    if written.is_ok() {
                        written = started.write_marker(0xe2, payload);
                    }
                });
                written?;
            }
            Ok(())
        };
        header().map_err(|payload| failure(&io_error, payload))?;
        Ok(Self {
            started: Some(started),
            io_error,
            passes,
            stride: width as usize * 4,
            height: height as usize,
            written: 0,
            pending: Vec::new(),
        })
    }

    /// libjpeg's passes over the image: 1 for a baseline encode, more for an optimised one.
    pub fn passes(&self) -> u32 {
        self.passes
    }

    /// Where the rows have got: `rows` handed to libjpeg, in the first of the passes.
    fn at_row(&self, rows: usize) -> Progress {
        let share = if self.height == 0 {
            0.0
        } else {
            rows as f64 / self.height as f64
        };
        Progress {
            rows,
            pass: 0,
            passes: self.passes,
            fraction: share / f64::from(self.passes),
        }
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
        step: &mut dyn FnMut(Progress) -> Result<(), E>,
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
        if completes && let Err(error) = step(self.at_row(self.height)) {
            self.abandon();
            return Err(error);
        }
        Ok(())
    }

    /// Hand libjpeg one strip of whole rows, after the caller's step.
    fn strip<E: From<JpegError>>(
        &mut self,
        rows: &[u8],
        step: &mut dyn FnMut(Progress) -> Result<(), E>,
    ) -> Result<(), E> {
        if let Err(error) = step(self.at_row(self.written)) {
            self.abandon();
            return Err(error);
        }
        let stride = self.stride;
        let Some(started) = self.started.as_mut() else {
            return Err(ended().into());
        };
        match started.write_scanlines(rows, stride) {
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
        self.started = None;
    }

    /// Finish the file: libjpeg runs the passes an optimised encode has left, writes what it still
    /// buffers and the trailer, and flushes the writer. Every row must have been given; a session
    /// missing rows, or one that has ended, is refused as [`JpegError::Internal`] and abandoned,
    /// so nothing more is written. The passes cannot be stopped; [`Self::finish_with`] can.
    pub fn finish(self) -> Result<(), JpegError> {
        self.finish_with(&mut |_| Ok::<(), JpegError>(()))
    }

    /// [`Self::finish`], reporting an optimised encode's passes to `step` as libjpeg's progress
    /// monitor counts them: at the start of each pass and as it advances, at most once per 1/1024
    /// of the whole encode, then once at completion with a fraction of 1. An error from `step`
    /// stops the encode between those reports, abandoning it as [`Self::abort`] does, and is
    /// returned as it is: libjpeg's call is left through its error path, as its own errors leave
    /// it. A baseline encode has no passes left, so `step` is not called.
    pub fn finish_with<E: From<JpegError>>(
        mut self,
        step: &mut dyn FnMut(Progress) -> Result<(), E>,
    ) -> Result<(), E> {
        let Some(mut started) = self.started.take() else {
            return Err(ended().into());
        };
        if self.written != self.height {
            return Err(
                JpegError::Internal("jpeg encode: finished before the last row".into()).into(),
            );
        }
        let rows = self.height;
        let mut stop = None;
        let mut last = self.at_row(rows);
        let finished = {
            let mut hook = |passes: Passes| {
                let within = if passes.limit == 0 {
                    0.0
                } else {
                    (passes.counter as f64 / passes.limit as f64).min(1.0)
                };
                let fraction = ((f64::from(passes.completed) + within) / f64::from(passes.total))
                    .clamp(last.fraction, 1.0);
                if passes.completed == last.pass && fraction - last.fraction < 1.0 / 1024.0 {
                    return true;
                }
                last = Progress {
                    rows,
                    pass: passes.completed,
                    passes: passes.total,
                    fraction,
                };
                match step(last) {
                    Ok(()) => true,
                    Err(error) => {
                        stop = Some(error);
                        false
                    }
                }
            };
            started.finish(&mut hook)
        };
        // Destroyed whatever happened: the file is complete, or libjpeg left it undefined.
        drop(started);
        if let Err(payload) = finished {
            if payload.is::<Stopped>()
                && let Some(error) = stop
            {
                return Err(error);
            }
            return Err(failure(&self.io_error, payload).into());
        }
        if self.passes > 1 {
            step(Progress {
                rows,
                pass: self.passes,
                passes: self.passes,
                fraction: 1.0,
            })?;
        }
        Ok(())
    }

    /// Abandon the encode as a step's error does: libjpeg's buffered bytes are discarded and the
    /// writer keeps only what was already written to it, an incomplete file whose owner removes it.
    pub fn abort(mut self) {
        self.abandon();
    }
}

/// Encode the `width` × `height` RGBA8 `rgba` (alpha ignored) into `out`, baseline or optimised as
/// `settings` say: an [`Encoder`] given the whole frame as one band, so libjpeg reads it in place,
/// [`STRIP_ROWS`] rows per call, then finished with [`Encoder::finish_with`]. `step` is called with
/// the progress before each call and once after the last, and through an optimised encode's passes;
/// an error from it abandons the encode, discarding what libjpeg still buffers, and is returned as
/// it is. A writer failure is [`JpegError::Write`] with the writer's error, a libjpeg error
/// [`JpegError::Encode`] with libjpeg's message; a frame that does not match its dimensions, a
/// segment too large for one marker and an ICC profile that is empty or too large for 255 chunks
/// are refused before anything is written.
pub fn encode<W: Write, E: From<JpegError>>(
    out: W,
    width: u32,
    height: u32,
    rgba: &[u8],
    settings: &Settings<'_>,
    step: &mut dyn FnMut(Progress) -> Result<(), E>,
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
    encoder.finish_with(step)
}
