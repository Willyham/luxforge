//! Bounded JPEG XL unpacking for a catalogued integer linear DNG.
//! LibRaw supplies identify-time metadata only; no pixel colour transform is requested.
use crate::{MAX_SOURCE_BYTES, NativeMetadata, RawError};
use jxl_oxide::{AllocTracker, InitializeResult, JxlImage, JxlThreadPool};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) fn decode_into(
    bytes: &[u8],
    native: &NativeMetadata,
    output: &mut [u16],
    cancel: &AtomicBool,
) -> Result<(), RawError> {
    let data = crate::format::dng_jxl_segment(bytes, native)?;
    let error = |error: Box<dyn std::error::Error + Send + Sync>| {
        RawError::Native(format!("JPEG XL: {error}"))
    };
    // The process's one global Rayon pool, never a pool of the decoder's own (performance rule 9).
    // Named here rather than left to the builder's default, so that a build without jxl-oxide's
    // `rayon` feature, which would decode serially, does not compile.
    let mut uninit = JxlImage::builder()
        .pool(JxlThreadPool::rayon_global())
        .alloc_tracker(AllocTracker::with_limit(MAX_SOURCE_BYTES))
        .build_uninit();
    let mut offset = 0;
    let mut image = loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        let end = (offset + 4096).min(data.len());
        if end == offset || end > 1024 * 1024 {
            return Err(RawError::InvalidInput("JPEG XL bounded header"));
        }
        let used = uninit.feed_bytes(&data[offset..end]).map_err(error)?;
        if used == 0 {
            return Err(RawError::InvalidInput("JPEG XL stalled header"));
        }
        offset += used;
        match uninit.try_init().map_err(error)? {
            InitializeResult::Initialized(image) => break image,
            InitializeResult::NeedMoreData(next) => uninit = next,
        }
    };
    let header = image.image_header();
    if image.width() != native.width
        || image.height() != native.height
        || header.metadata.orientation != 1
        || header.metadata.animation.is_some()
        || !header.metadata.ec_info.is_empty()
        || image.pixel_format() != jxl_oxide::PixelFormat::Rgb
        || !matches!(
            header.metadata.bit_depth,
            jxl_image::BitDepth::IntegerSample {
                bits_per_sample: 16
            }
        )
    {
        return Err(RawError::UnsupportedMode(
            "JPEG XL DNG geometry/channels/depth".into(),
        ));
    }
    while offset < data.len() {
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        let end = (offset + 32768).min(data.len());
        let used = image.feed_bytes(&data[offset..end]).map_err(error)?;
        if used == 0 {
            return Err(RawError::InvalidInput("JPEG XL stalled codestream"));
        }
        offset += used;
    }
    image.finalize().map_err(error)?;
    if !image.is_loading_done()
        || image.num_loaded_keyframes() != 1
        || image.num_loaded_frames() != 1
    {
        return Err(RawError::InvalidInput("JPEG XL incomplete/multiple frames"));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(RawError::Cancelled);
    }
    let rendered = image.render_frame(0).map_err(error)?;
    let mut stream = rendered.stream();
    if stream.channels() != 3 || output.len() != native.width as usize * native.height as usize * 3
    {
        return Err(RawError::InvalidInput("JPEG XL output shape"));
    }
    for row in output.chunks_mut(native.width as usize * 3) {
        if cancel.load(Ordering::Relaxed) {
            return Err(RawError::Cancelled);
        }
        if stream.write_to_buffer(row) != row.len() {
            return Err(RawError::InvalidInput("JPEG XL short output"));
        }
    }
    Ok(())
}
