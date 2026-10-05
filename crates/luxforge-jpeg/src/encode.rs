//! One baseline JPEG encode, streamed into the caller's writer a strip at a time.

use crate::{
    JpegError, MAX_SEGMENT_PAYLOAD, STRIP_ROWS,
    decode::{guarded, panic_message},
    icc,
};
use mozjpeg::{ColorSpace, Compress, Marker, PixelDensity, PixelDensityUnit};
use std::{
    any::Any,
    cell::RefCell,
    io::{self, Write},
};

/// How [`encode`] compresses: libjpeg's quality from 1 to 100, and the chroma sampling as the
/// pixels one chroma sample covers horizontally and vertically, `(1, 1)` for 4:4:4.
pub struct Settings<'a> {
    pub quality: u8,
    pub chroma: (u8, u8),
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
/// encode, discarding what libjpeg still buffers, and is returned as it is. A writer failure is
/// [`JpegError::Write`] with the writer's error, a libjpeg error [`JpegError::Encode`] with
/// libjpeg's message; a frame that does not match its dimensions, a segment too large for one
/// marker and an ICC profile that is empty or too large for 255 chunks are refused before anything
/// is written.
pub fn encode<W: Write, E: From<JpegError>>(
    out: W,
    width: u32,
    height: u32,
    rgba: &[u8],
    settings: &Settings<'_>,
    step: &mut dyn FnMut(usize) -> Result<(), E>,
) -> Result<(), E> {
    let stride = width as usize * 4;
    let rows = height as usize;
    if rgba.len() != stride * rows {
        return Err(JpegError::Internal(
            "jpeg encode: the frame's pixels do not match its dimensions".into(),
        )
        .into());
    }
    if settings
        .segments
        .iter()
        .any(|(_, payload)| payload.len() > MAX_SEGMENT_PAYLOAD)
    {
        return Err(JpegError::Internal(
            "jpeg encode: a segment does not fit one APPn marker".into(),
        )
        .into());
    }
    let icc_chunks = settings.icc.map(icc::chunk_count).transpose()?;
    let io_error = RefCell::<Option<io::Error>>::new(None);
    let failure = |payload: Box<dyn Any + Send>| -> E {
        if let Some(error) = io_error.borrow_mut().take() {
            return JpegError::Write(error).into();
        }
        JpegError::Encode(panic_message(&payload).to_owned()).into()
    };
    let refused = |error: io::Error| -> E { JpegError::Encode(error.to_string()).into() };
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
        if let Some(density) = settings.pixels_per_inch {
            compress.set_pixel_density(PixelDensity {
                unit: PixelDensityUnit::Inches,
                x: density,
                y: density,
            });
        }
        compress.start_compress(writer)
    })
    .map_err(&failure)?
    .map_err(refused)?;
    // `start_compress` has written SOI and the JFIF APP0; the other segments follow it in order,
    // then the ICC chunks.
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
