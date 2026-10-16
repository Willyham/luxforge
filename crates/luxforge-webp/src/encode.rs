//! The encode session: libwebp's config, picture, progress hook and output writer, then the mux.
//! Every `unsafe` call into libwebp in the crate is in this file.

use crate::{Error, MAX_SIDE, Settings};
use libwebp_sys as webp;
use std::{
    any::Any,
    ffi::{c_int, c_void},
    mem::MaybeUninit,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    ptr,
};

/// libwebp's smallest side for its sharp conversion (`kMinDimensionIterativeConversion`): a smaller
/// picture takes the plain conversion even when sharp is asked for.
const MIN_SHARP_SIDE: u32 = 4;

/// `sharpyuv`'s `kSharpYuvMatrixWebp`: the conversion matrix libwebp's own sharp conversion uses.
const SHARP_YUV_MATRIX_WEBP: c_int = 0;

// libwebp's `sharpyuv` library, compiled into the same static library as the encoder by
// `libwebp-sys`, which binds only the `WebP*` names. Its public header is `sharpyuv/sharpyuv.h`.
#[allow(unsafe_code)]
unsafe extern "C" {
    fn SharpYuvConvert(
        r_ptr: *const c_void,
        g_ptr: *const c_void,
        b_ptr: *const c_void,
        rgb_step: c_int,
        rgb_stride: c_int,
        rgb_bit_depth: c_int,
        y_ptr: *mut c_void,
        y_stride: c_int,
        u_ptr: *mut c_void,
        u_stride: c_int,
        v_ptr: *mut c_void,
        v_stride: c_int,
        yuv_bit_depth: c_int,
        width: c_int,
        height: c_int,
        yuv_matrix: *const c_void,
    ) -> c_int;
    fn SharpYuvGetConversionMatrix(matrix_type: c_int) -> *const c_void;
}

/// Encode `rgba`, a `width` × `height` frame of opaque RGBA8 rows, as a lossy WebP file
/// (the crate's contract).
pub fn encode(
    width: u32,
    height: u32,
    rgba: &[u8],
    settings: &Settings<'_>,
    progress: &mut dyn FnMut(u8) -> bool,
) -> Result<Vec<u8>, Error> {
    check(width, height, rgba, settings)?;
    let sharp = settings.optimise && width >= MIN_SHARP_SIDE && height >= MIN_SHARP_SIDE;
    // libwebp's last report may stop short of 100 (97, say); the assembled file is the 100.
    let mut last = 0;
    let bitstream = vp8(
        width,
        height,
        rgba,
        settings,
        Conversion::of(sharp),
        &mut |percent| {
            last = percent;
            progress(percent)
        },
    )?;
    let file = assemble(&bitstream, settings.icc, settings.exif)?;
    if last < 100 && !progress(100) {
        return Err(Error::Stopped);
    }
    Ok(file)
}

fn check(width: u32, height: u32, rgba: &[u8], settings: &Settings<'_>) -> Result<(), Error> {
    if width == 0 || height == 0 {
        return Err(Error::Invalid("the frame is empty"));
    }
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::Invalid("a side is over 16,383 pixels"));
    }
    if settings.quality > 100 {
        return Err(Error::Invalid("the quality is over 100"));
    }
    // Both sides are at most 16,383, so the product fits any usize of at least 32 bits.
    if rgba.len() != width as usize * height as usize * 4 {
        return Err(Error::Invalid(
            "the frame is not width × height RGBA8 pixels",
        ));
    }
    Ok(())
}

/// How the frame becomes libwebp's YUV planes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Conversion {
    /// libwebp's picture import, straight into the planes.
    Plain,
    /// `sharpyuv` on the borrowed frame, straight into the planes.
    Sharp,
    /// libwebp's own sharp path: an ARGB copy of the frame that the encoder converts. Only the
    /// tests use it, as the reference [`Conversion::Sharp`] matches.
    #[cfg(test)]
    SharpThroughArgb,
}

impl Conversion {
    fn of(sharp: bool) -> Self {
        if sharp { Self::Sharp } else { Self::Plain }
    }
}

/// What the progress hook reaches through the picture's `user_data`.
struct Hook<'a> {
    progress: &'a mut dyn FnMut(u8) -> bool,
    panic: Option<Box<dyn Any + Send>>,
}

/// What the writer reaches through the picture's `custom_ptr`.
struct Sink {
    bytes: Vec<u8>,
    panic: Option<Box<dyn Any + Send>>,
}

/// libwebp's picture, freed when dropped.
struct Picture(webp::WebPPicture);

impl Drop for Picture {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the picture was initialised by `WebPPictureInitInternal` and its memory, if any,
        // allocated by libwebp; freeing it twice is not possible, since this runs once, and
        // `WebPPictureFree` accepts a picture that owns nothing.
        unsafe { webp::WebPPictureFree(&mut self.0) };
    }
}

/// The VP8 bitstream of the frame: libwebp's config and picture, the picture filled by
/// `conversion`, encoded with the hook and writer below.
#[allow(unsafe_code)]
fn vp8(
    width: u32,
    height: u32,
    rgba: &[u8],
    settings: &Settings<'_>,
    conversion: Conversion,
    progress: &mut dyn FnMut(u8) -> bool,
) -> Result<Vec<u8>, Error> {
    let mut config = MaybeUninit::<webp::WebPConfig>::uninit();
    // SAFETY: `WebPConfigInitInternal` initialises every field of the config it is given and
    // returns 0 only for an ABI mismatch, when it writes nothing that is read.
    let initialised = unsafe {
        webp::WebPConfigInitInternal(
            config.as_mut_ptr(),
            webp::WebPPreset::WEBP_PRESET_DEFAULT,
            f32::from(settings.quality),
            webp::WEBP_ENCODER_ABI_VERSION as c_int,
        )
    };
    if initialised == 0 {
        return Err(Error::Encode(
            "libwebp's encoder ABI does not match its header",
        ));
    }
    // SAFETY: initialised above.
    let mut config = unsafe { config.assume_init() };
    config.lossless = 0;
    config.method = if settings.optimise { 6 } else { 4 };
    config.use_sharp_yuv = c_int::from(settings.optimise);
    // One thread: the encode runs on the caller's thread, with no worker of libwebp's own.
    config.thread_level = 0;
    // SAFETY: the config is initialised.
    if unsafe { webp::WebPValidateConfig(&config) } == 0 {
        return Err(Error::Encode("libwebp refused the configuration"));
    }

    let mut picture = MaybeUninit::<webp::WebPPicture>::uninit();
    // SAFETY: as for the config: every field is written, and 0 means an ABI mismatch.
    if unsafe {
        webp::WebPPictureInitInternal(
            picture.as_mut_ptr(),
            webp::WEBP_ENCODER_ABI_VERSION as c_int,
        )
    } == 0
    {
        return Err(Error::Encode(
            "libwebp's picture ABI does not match its header",
        ));
    }
    // SAFETY: initialised above; from here the wrapper frees whatever libwebp allocates in it.
    let mut picture = Picture(unsafe { picture.assume_init() });
    let pic = &mut picture.0;
    // `check` bounds both sides by 16,383 and the stride by 65,532.
    pic.width = width as c_int;
    pic.height = height as c_int;
    let stride = (width * 4) as c_int;
    pic.colorspace = webp::WebPEncCSP::WEBP_YUV420;
    #[cfg(test)]
    let through_argb = conversion == Conversion::SharpThroughArgb;
    #[cfg(not(test))]
    let through_argb = false;
    pic.use_argb = c_int::from(through_argb);
    let filled = match conversion {
        // SAFETY: `rgba` holds `height` rows of `stride` bytes (`check`), which the import reads
        // and never keeps; it allocates the picture's planes itself. RGBX skips the fourth byte.
        Conversion::Plain => unsafe { webp::WebPPictureImportRGBX(pic, rgba.as_ptr(), stride) },
        Conversion::Sharp => {
            // SAFETY: the picture is initialised with its size and a YUV 4:2:0 colourspace, so
            // `WebPPictureAlloc` allocates exactly its Y, U and V planes.
            if unsafe { webp::WebPPictureAlloc(pic) } == 0 {
                return Err(error(pic.error_code));
            }
            // SAFETY: the three channel pointers read `height` rows of `stride` bytes at a step
            // of 4 from offsets 0, 1 and 2 of `rgba`, which holds them (`check`); the planes and
            // strides are the ones `WebPPictureAlloc` just allocated for this size, which the
            // conversion writes and never keeps. The matrix is a static table of the library.
            unsafe {
                let base = rgba.as_ptr();
                SharpYuvConvert(
                    base.cast(),
                    base.add(1).cast(),
                    base.add(2).cast(),
                    4,
                    stride,
                    8,
                    pic.y.cast(),
                    pic.y_stride,
                    pic.u.cast(),
                    pic.uv_stride,
                    pic.v.cast(),
                    pic.uv_stride,
                    8,
                    pic.width,
                    pic.height,
                    SharpYuvGetConversionMatrix(SHARP_YUV_MATRIX_WEBP),
                )
            }
        }
        #[cfg(test)]
        // SAFETY: as for `Plain`; with `use_argb` the import copies the frame into ARGB.
        Conversion::SharpThroughArgb => unsafe {
            webp::WebPPictureImportRGBX(pic, rgba.as_ptr(), stride)
        },
    };
    if filled == 0 {
        return Err(Error::Encode(
            "libwebp could not convert the frame (out of memory)",
        ));
    }

    let mut hook = Hook {
        progress,
        panic: None,
    };
    let mut sink = Sink {
        bytes: Vec::new(),
        panic: None,
    };
    pic.progress_hook = Some(report);
    pic.user_data = ptr::from_mut(&mut hook).cast();
    pic.writer = Some(write);
    pic.custom_ptr = ptr::from_mut(&mut sink).cast();
    // SAFETY: the config is valid and the picture holds its planes; the hook and writer read
    // `user_data` and `custom_ptr` as the `Hook` and `Sink` above, which outlive the call and
    // are not touched by anything else until it returns.
    let encoded = unsafe { webp::WebPEncode(&config, pic) };
    let code = pic.error_code;
    // The hook and the writer are not called again.
    pic.progress_hook = None;
    pic.user_data = ptr::null_mut();
    pic.writer = None;
    pic.custom_ptr = ptr::null_mut();
    if let Some(panic) = hook.panic.or(sink.panic) {
        resume_unwind(panic);
    }
    if encoded == 0 {
        return Err(match code {
            webp::WebPEncodingError::VP8_ENC_ERROR_USER_ABORT => Error::Stopped,
            code => error(code),
        });
    }
    Ok(sink.bytes)
}

/// libwebp's progress hook: the caller's progress, through the picture's `user_data`.
#[allow(unsafe_code)]
unsafe extern "C" fn report(percent: c_int, picture: *const webp::WebPPicture) -> c_int {
    // SAFETY: libwebp calls the hook only during `WebPEncode` in `vp8`, with that call's picture,
    // whose `user_data` points at its `Hook`, borrowed by nothing else during the call.
    let hook = unsafe { &mut *(*picture).user_data.cast::<Hook<'_>>() };
    if hook.panic.is_some() {
        return 0;
    }
    let percent = percent.clamp(0, 100) as u8;
    match catch_unwind(AssertUnwindSafe(|| (hook.progress)(percent))) {
        Ok(go_on) => c_int::from(go_on),
        Err(panic) => {
            hook.panic = Some(panic);
            0
        }
    }
}

/// libwebp's writer: the bitstream appended to the `Sink` behind the picture's `custom_ptr`.
#[allow(unsafe_code)]
unsafe extern "C" fn write(
    data: *const u8,
    size: usize,
    picture: *const webp::WebPPicture,
) -> c_int {
    // SAFETY: as for `report`, with `custom_ptr` and the call's `Sink`.
    let sink = unsafe { &mut *(*picture).custom_ptr.cast::<Sink>() };
    if size == 0 {
        return 1;
    }
    // SAFETY: libwebp hands the writer `size` readable bytes at `data`, valid for this call.
    let data = unsafe { std::slice::from_raw_parts(data, size) };
    match catch_unwind(AssertUnwindSafe(|| sink.bytes.extend_from_slice(data))) {
        Ok(()) => 1,
        Err(panic) => {
            sink.panic = Some(panic);
            0
        }
    }
}

fn error(code: webp::WebPEncodingError) -> Error {
    use webp::WebPEncodingError as Code;
    Error::Encode(match code {
        Code::VP8_ENC_ERROR_OUT_OF_MEMORY => "out of memory",
        Code::VP8_ENC_ERROR_BITSTREAM_OUT_OF_MEMORY => "out of memory for the bitstream",
        Code::VP8_ENC_ERROR_NULL_PARAMETER => "a required input is missing",
        Code::VP8_ENC_ERROR_INVALID_CONFIGURATION => "the configuration is invalid",
        Code::VP8_ENC_ERROR_BAD_DIMENSION => "the picture's size is invalid",
        Code::VP8_ENC_ERROR_PARTITION0_OVERFLOW => "the first partition is over 512 KiB",
        Code::VP8_ENC_ERROR_PARTITION_OVERFLOW => "a partition is over 16 MiB",
        Code::VP8_ENC_ERROR_BAD_WRITE => "the output could not be written",
        Code::VP8_ENC_ERROR_FILE_TOO_BIG => "the file is over 4 GiB",
        Code::VP8_ENC_ERROR_USER_ABORT => "stopped",
        Code::VP8_ENC_OK | Code::VP8_ENC_ERROR_LAST => "an unknown error",
    })
}

/// libwebp's mux, deleted when dropped.
struct Mux(*mut webp::WebPMux);

impl Drop for Mux {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // SAFETY: the pointer came from `WebPMuxNew`, is not null, and is deleted only here.
        unsafe { webp::WebPMuxDelete(self.0) };
    }
}

/// The file: `bitstream` in a container with the `ICCP` and `EXIF` chunks, assembled by the mux.
#[allow(unsafe_code)]
fn assemble(bitstream: &[u8], icc: Option<&[u8]>, exif: Option<&[u8]>) -> Result<Vec<u8>, Error> {
    let raw = webp::WebPMuxNew();
    if raw.is_null() {
        return Err(Error::Mux("start", "out of memory"));
    }
    let mux = Mux(raw);
    let data = |bytes: &[u8]| webp::WebPData {
        bytes: bytes.as_ptr(),
        size: bytes.len(),
    };
    // SAFETY: the mux is live; with `copy_data` 0 it references `bitstream` and the chunks, which
    // outlive it, since it is deleted at the end of this function.
    let status = unsafe { webp::WebPMuxSetImage(mux.0, &data(bitstream), 0) };
    mux_status("set the image", status)?;
    for (fourcc, chunk) in [(c"ICCP", icc), (c"EXIF", exif)] {
        let Some(chunk) = chunk.filter(|chunk| !chunk.is_empty()) else {
            continue;
        };
        // SAFETY: as above; `fourcc` is a NUL-terminated four-character code.
        let status = unsafe { webp::WebPMuxSetChunk(mux.0, fourcc.as_ptr(), &data(chunk), 0) };
        mux_status("add a chunk", status)?;
    }
    let mut assembled = webp::WebPData {
        bytes: ptr::null(),
        size: 0,
    };
    // SAFETY: the mux is live and `assembled` is empty, as `WebPMuxAssemble` requires.
    let status = unsafe { webp::WebPMuxAssemble(mux.0, &mut assembled) };
    let file = if assembled.bytes.is_null() {
        Vec::new()
    } else {
        // SAFETY: on success the mux allocated `size` bytes at `bytes` for the caller, who frees
        // them with `WebPFree`, once, after copying them out.
        unsafe {
            let file = std::slice::from_raw_parts(assembled.bytes, assembled.size).to_vec();
            webp::WebPFree(assembled.bytes.cast_mut().cast());
            file
        }
    };
    mux_status("assemble the file", status)?;
    Ok(file)
}

fn mux_status(step: &'static str, status: webp::WebPMuxError) -> Result<(), Error> {
    use webp::WebPMuxError as Status;
    let error = match status {
        Status::WEBP_MUX_OK => return Ok(()),
        Status::WEBP_MUX_NOT_FOUND => "not found",
        Status::WEBP_MUX_INVALID_ARGUMENT => "an invalid argument",
        Status::WEBP_MUX_BAD_DATA => "bad data",
        Status::WEBP_MUX_MEMORY_ERROR => "out of memory",
        Status::WEBP_MUX_NOT_ENOUGH_DATA => "not enough data",
    };
    Err(Error::Mux(step, error))
}

#[cfg(test)]
mod tests;
