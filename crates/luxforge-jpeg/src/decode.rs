//! One JPEG decode: the container walked and checked, libjpeg's header read and checked, then RGBA
//! rows on request, at full scale or one of libjpeg's DCT scales, with the decoder's own error
//! manager deciding which libjpeg warnings stop it. The error manager and the frame checks serve
//! the region decode's session too.

use crate::{JpegError, Limits, Scale, container, icc, warnings};
use mozjpeg::{ColorSpace, Decompress, Marker, decompress::DecompressStarted};
use mozjpeg_sys::{jpeg_common_struct, jpeg_error_mgr};
use std::{
    any::Any,
    os::raw::c_int,
    panic::{self, AssertUnwindSafe},
    ptr,
};

/// libjpeg's `DSTATE_INHEADER` (`jpegint.h`, not in the bindings): reading the header's markers,
/// before the first scan. The states before it are 200 and those after it higher.
const DSTATE_INHEADER: c_int = 201;

/// Runs one call into libjpeg, catching the unwind by which it reports an error.
pub(crate) fn guarded<T>(call: impl FnOnce() -> T) -> Result<T, Box<dyn Any + Send>> {
    panic::catch_unwind(AssertUnwindSafe(call))
}

/// What a decode's error manager unwinds with.
enum Stop {
    /// libjpeg's `error_exit`: data it cannot decode or does not support.
    Fatal,
    /// libjpeg's `emit_message` at warning level with a code the warning table refuses.
    Warning(c_int),
}

/// libjpeg's message code for the warning or error being raised on `cinfo`.
#[allow(unsafe_code)]
fn message_code(cinfo: &jpeg_common_struct) -> c_int {
    // SAFETY: libjpeg calls the error manager only with a session's own `cinfo`, whose `err`
    // points at the manager `error_manager()` built, on the heap and owned by that session until
    // after it is destroyed, set before any call into libjpeg: by `mozjpeg` for a `Decoder`, by
    // `Session::open` for a region. libjpeg's raise macros and `mozjpeg`'s own source manager set
    // `msg_code` before they call. A session runs on one thread, so nothing writes the field while
    // it is read.
    unsafe { (*cinfo.err).msg_code }
}

#[cfg(test)]
thread_local! {
    /// The warnings the tests' decodes went on past, on this thread.
    pub(crate) static ACCEPTED: std::cell::RefCell<Vec<c_int>> = const {
        std::cell::RefCell::new(Vec::new())
    };
}

extern "C-unwind" fn stop_on_error(_: &mut jpeg_common_struct) {
    panic::resume_unwind(Box::new(Stop::Fatal));
}

/// A warning (level -1) the table refuses stops the decode; one it accepts, and every trace message
/// (level 0 and above), lets libjpeg go on.
extern "C-unwind" fn on_message(cinfo: &mut jpeg_common_struct, level: c_int) {
    if level >= 0 {
        return;
    }
    let code = message_code(cinfo);
    if !warnings::accepts(code, cinfo.global_state <= DSTATE_INHEADER) {
        panic::resume_unwind(Box::new(Stop::Warning(code)));
    }
    #[cfg(test)]
    ACCEPTED.with(|accepted| accepted.borrow_mut().push(code));
}

extern "C-unwind" fn silent(_: &mut jpeg_common_struct) {}

extern "C-unwind" fn no_text(_: &mut jpeg_common_struct, _: &[u8; 80]) {}

/// The decoder's error manager: every error and every refused warning unwinds, nothing is printed.
/// libjpeg calls only `error_exit`, `emit_message` and `reset_error_mgr` itself; the message
/// tables are read only by the formatting functions replaced here, so they stay empty.
pub(crate) fn error_manager() -> jpeg_error_mgr {
    jpeg_error_mgr {
        error_exit: Some(stop_on_error),
        emit_message: Some(on_message),
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
pub(crate) fn decode_failure(payload: Box<dyn Any + Send>) -> JpegError {
    match payload.downcast_ref::<Stop>() {
        Some(Stop::Warning(code)) => JpegError::Corrupt(*code),
        Some(Stop::Fatal) => JpegError::Undecodable,
        // A panic in the bindings themselves, not libjpeg's.
        None => JpegError::Internal(format!("jpeg decode: {}", panic_message(&payload))),
    }
}

pub(crate) fn panic_message(payload: &Box<dyn Any + Send>) -> &str {
    payload
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| payload.downcast_ref::<&str>().copied())
        .unwrap_or("libjpeg failed")
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
pub struct Decoder<'a> {
    session: Session<'a>,
    /// The frame's size, as libjpeg's header gives it.
    frame: (u32, u32),
    /// The size decoded: the frame's, or its size at the scale set.
    width: u32,
    height: u32,
    components: u8,
    icc: Option<Vec<u8>>,
    rows_read: u32,
}

impl<'a> Decoder<'a> {
    /// Walk the container ([`container::header`]) and check the frame it declares against
    /// `limits` and the supported colour spaces (greyscale, and three-component YCbCr or RGB);
    /// then read libjpeg's header, check its reading of the frame the same way, and reassemble the
    /// ICC profile. Decompression starts with the first [`Self::read_rows`], so a caller can
    /// refuse the profile or choose a scale first; a multi-scan frame is decoded into libjpeg's
    /// coefficient buffer then, at the frame's full size whatever the scale.
    pub fn new(bytes: &'a [u8], limits: Limits) -> Result<Self, JpegError> {
        check_declared(bytes, limits)?;
        let header = guarded(|| {
            Decompress::builder()
                .with_err(error_manager())
                .with_markers(&[Marker::APP(2)])
                .from_mem(bytes)
        })
        .map_err(decode_failure)?
        .map_err(|error| JpegError::Malformed(format!("JPEG header: {error}")))?;
        let (width, height) = header.size();
        let (width, height) = (width as u32, height as u32);
        check_frame(width, height, limits)?;
        let components = supported_components(header.components().len(), header.color_space())?;
        let icc = icc::reassemble(header.markers().map(|marker| marker.data))?;
        Ok(Self {
            session: Session::Header(header),
            frame: (width, height),
            width,
            height,
            components,
            icc,
            rows_read: 0,
        })
    }

    /// The width decoded: the frame's, or its width at the scale set.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The height decoded: the frame's, or its height at the scale set.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Decode at `scale` ([`Scale::covering`] chooses one for a target size): from then on
    /// [`Self::width`] and [`Self::height`] report the scaled output, whose rows
    /// [`Self::read_rows`] writes. Only before the first row; a later call sets the scale again
    /// from the frame's size. The header was checked against the caller's [`Limits`] at the
    /// frame's full size, which bounds what libjpeg allocates whatever the scale: its row buffers
    /// scale with the output, a multi-scan frame's coefficient buffer with the full frame.
    pub fn set_scale(&mut self, scale: Scale) -> Result<(), JpegError> {
        let Session::Header(header) = &mut self.session else {
            return Err(JpegError::Internal(
                "jpeg decode: the scale is set before the first row".into(),
            ));
        };
        header.scale(scale.numerator());
        (self.width, self.height) = scale.output(self.frame.0, self.frame.1);
        Ok(())
    }

    /// 1 for greyscale, 3 for colour.
    pub fn components(&self) -> u8 {
        self.components
    }

    /// The reassembled ICC profile, when the file carries one.
    pub fn icc_profile(&self) -> Option<&[u8]> {
        self.icc.as_deref()
    }

    /// Decode the next whole rows into `rows`, RGBA8, `width * 4` bytes each.
    pub fn read_rows(&mut self, rows: &mut [u8]) -> Result<(), JpegError> {
        let stride = self.width as usize * 4;
        let count = rows.len() / stride;
        if !rows.len().is_multiple_of(stride) || count > (self.height - self.rows_read) as usize {
            return Err(JpegError::Internal(
                "jpeg decode: rows requested beyond the image or not whole rows".into(),
            ));
        }
        let result = self.read_into(rows);
        match result {
            Ok(()) => self.rows_read += count as u32,
            Err(_) => self.session = Session::Ended,
        }
        result
    }

    fn read_into(&mut self, rows: &mut [u8]) -> Result<(), JpegError> {
        if let Session::Header(_) = self.session {
            let Session::Header(header) = std::mem::replace(&mut self.session, Session::Ended)
            else {
                unreachable!("matched above");
            };
            let started = guarded(move || header.rgba())
                .map_err(decode_failure)?
                .map_err(|error| JpegError::Malformed(format!("JPEG decode: {error}")))?;
            // libjpeg's output size must be the header's at the scale set (`Scale::output`).
            if (started.width(), started.height()) != (self.width as usize, self.height as usize) {
                return Err(JpegError::Internal(
                    "jpeg decode: output size differs from the header's at its scale".into(),
                ));
            }
            self.session = Session::Started(started);
        }
        let Session::Started(started) = &mut self.session else {
            return Err(JpegError::Internal(
                "jpeg decode: the session has ended".into(),
            ));
        };
        match guarded(|| started.read_scanlines_into::<u8>(rows)) {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(JpegError::Malformed(format!("JPEG scan data: {error}"))),
            Err(payload) => Err(decode_failure(payload)),
        }
    }

    /// Read the rest of the file to EOI, so a warning about data after the last row is judged too,
    /// and end the session. Every row must have been read.
    pub fn finish(mut self) -> Result<(), JpegError> {
        if self.rows_read != self.height {
            return Err(JpegError::Internal(
                "jpeg decode: finished before the last row".into(),
            ));
        }
        let Session::Started(started) = std::mem::replace(&mut self.session, Session::Ended) else {
            return Err(JpegError::Internal(
                "jpeg decode: the session has ended".into(),
            ));
        };
        guarded(move || started.finish())
            .map_err(decode_failure)?
            .map_err(|error| JpegError::Malformed(format!("JPEG end: {error}")))
    }
}

/// Walk the container ([`container::header`]) and check the frame it declares against `limits`
/// and the component counts the decoder supports, before libjpeg reads anything.
pub(crate) fn check_declared(bytes: &[u8], limits: Limits) -> Result<container::Header, JpegError> {
    let declared = container::header(bytes)?;
    check_frame(declared.width, declared.height, limits)?;
    if ![1, 3].contains(&declared.components) {
        return Err(JpegError::ColourSpace);
    }
    Ok(declared)
}

/// The components a decode reports for libjpeg's reading of the frame: 1 for greyscale, 3 for
/// YCbCr or RGB. Any other colour space is refused.
pub(crate) fn supported_components(count: usize, space: ColorSpace) -> Result<u8, JpegError> {
    match (count, space) {
        (1, ColorSpace::JCS_GRAYSCALE) => Ok(1),
        (3, ColorSpace::JCS_YCbCr | ColorSpace::JCS_RGB) => Ok(3),
        _ => Err(JpegError::ColourSpace),
    }
}

/// A frame of at least one pixel and within `limits`.
pub(crate) fn check_frame(width: u32, height: u32, limits: Limits) -> Result<(), JpegError> {
    if width == 0
        || height == 0
        || width > limits.max_side
        || height > limits.max_side
        || u64::from(width) * u64::from(height) > limits.max_pixels
    {
        return Err(JpegError::Dimensions);
    }
    Ok(())
}
