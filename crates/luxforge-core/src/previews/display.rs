//! Decoding a cached preview for a client to draw. The desktop's Select grid reads a file's grid
//! tier through `preview.read`, which names the tier's JPEG under `<catalog>.index/previews/`, and
//! decodes it with [`decode_preview`] on a worker of its own — never on its update loop and never
//! on the catalog owner — at the size its cells need: at the DCT scale that covers the frame fitted
//! within that size, then box-downscaled to it with the proxy's own downscale, never enlarged.
//!
//! Every tier is stored upright, so its pixels are drawn as decoded; as the loupe shows a camera
//! preview, no ICC profile is applied. The JPEG is read whole, within [`MAX_PREVIEW_FILE_BYTES`] —
//! a cached tier is not an original, so it takes neither the source work's read nor its limits —
//! and decoded within the JPEG original's limits (`source::JPEG_LIMITS`); the one frame a decode
//! allocates is at the scale that covers the fitted size, and the downscale reads it once.
use crate::{
    Cancel, Error, PreviewSource, ProxyBounds, ProxyPlan, Raster, SourceImage, source::JPEG_LIMITS,
};
use luxforge_jpeg::{Decoder, STRIP_ROWS, Scale};
use std::{fs::File, io::Read, path::Path, sync::Arc};

/// The most bytes read of one cached preview. A grid tier is about 40 KB and a loupe tier, at most
/// 2560 px at quality 85, a few MB; a larger file is not a tier the lane wrote.
pub(crate) const MAX_PREVIEW_FILE_BYTES: u64 = 16 << 20;

/// A cached preview decoded for display: upright RGBA8 rows of `width * 4` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodedPreview {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Decode the cached preview JPEG at `path` fitted within `side` pixels a side (at least one), never
/// enlarged. `cancel` is checked before the file is read and between the decode's strips, so a
/// caller whose request was superseded stops at its next strip with `cancelled`.
pub fn decode_preview(path: &Path, side: u32, cancel: &Cancel) -> Result<DecodedPreview, Error> {
    cancel.check()?;
    let bytes = read_preview(path)?;
    cancel.check()?;
    let mut decoder = Decoder::new(&bytes, JPEG_LIMITS)?;
    let frame = (decoder.width(), decoder.height());
    let bounds = ProxyBounds {
        width: side.max(1),
        height: side.max(1),
    };
    let fitted =
        ProxyPlan::fit(frame, frame, bounds).map_or(frame, |plan| (plan.width, plan.height));
    decoder.set_scale(Scale::covering(frame, fitted))?;
    let (width, height) = (decoder.width(), decoder.height());
    let stride = width as usize * 4;
    let mut rgba = vec![0; Raster::expected_len(width, height)?];
    for strip in rgba.chunks_mut(STRIP_ROWS * stride) {
        cancel.check()?;
        decoder.read_rows(strip)?;
    }
    decoder.finish()?;
    let Some(plan) = ProxyPlan::fit((width, height), (width, height), bounds) else {
        return Ok(DecodedPreview {
            width,
            height,
            rgba,
        });
    };
    let decoded = PreviewSource::Jpeg(SourceImage {
        width,
        height,
        rgba: Arc::new(rgba),
        fingerprint: String::new(),
        orientation: 1,
        capture: Arc::default(),
    });
    let PreviewSource::Jpeg(fitted) = decoded.proxy_cancellable(plan, cancel)? else {
        return Err(Error::internal("a byte source downscaled to linear planes"));
    };
    drop(decoded);
    Ok(DecodedPreview {
        width: fitted.width,
        height: fitted.height,
        // The downscale's own new frame, which nothing else holds.
        rgba: Arc::try_unwrap(fitted.rgba).unwrap_or_else(|shared| shared.as_ref().clone()),
    })
}

/// The whole cached preview at `path`, refused past [`MAX_PREVIEW_FILE_BYTES`].
fn read_preview(path: &Path) -> Result<Vec<u8>, Error> {
    let access = |error: std::io::Error| Error::file_access(error.kind().to_string());
    let file = File::open(path).map_err(access)?;
    let too_large = || {
        Error::resource_limit(format!(
            "a cached preview past {MAX_PREVIEW_FILE_BYTES} bytes"
        ))
    };
    let len = file.metadata().map_err(access)?.len();
    if len > MAX_PREVIEW_FILE_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::with_capacity(len as usize);
    file.take(MAX_PREVIEW_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(access)?;
    if bytes.len() as u64 > MAX_PREVIEW_FILE_BYTES {
        return Err(too_large());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorKind;
    use luxforge_testbase::paths::temp_dir;
    use std::fs;

    /// Red, green, blue and yellow quadrants: top left, top right, bottom left, bottom right.
    const QUADRANTS: [[u8; 3]; 4] = [[220, 35, 45], [35, 190, 65], [40, 70, 220], [235, 195, 30]];

    fn quadrants_jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                let quadrant = usize::from(y >= height / 2) * 2 + usize::from(x >= width / 2);
                rgba.extend_from_slice(&QUADRANTS[quadrant]);
                rgba.push(255);
            }
        }
        let mut jpeg = Vec::new();
        luxforge_jpeg::encode::<_, Error>(
            &mut jpeg,
            width,
            height,
            &rgba,
            &luxforge_jpeg::Settings {
                quality: 95,
                chroma: (1, 1),
                segments: &[],
                icc: None,
                pixels_per_inch: None,
            },
            &mut |_| Ok(()),
        )
        .unwrap();
        jpeg
    }

    /// The colour at the centre of each quadrant, in [`QUADRANTS`]' order.
    fn quadrant_colours(preview: &DecodedPreview) -> [[u8; 3]; 4] {
        [(1, 1), (3, 1), (1, 3), (3, 3)].map(|(x, y)| {
            let at =
                ((preview.height * y / 4) * preview.width + preview.width * x / 4) as usize * 4;
            [preview.rgba[at], preview.rgba[at + 1], preview.rgba[at + 2]]
        })
    }

    /// A preview decodes at the DCT scale that covers the side asked for, and is box-downscaled to
    /// fit within it, never enlarged; its quadrants keep their colours.
    #[test]
    fn preview_display_decodes_fitted_within_the_side_never_enlarged() {
        let dir = temp_dir("preview-display");
        let path = dir.join("grid.jpg");
        fs::write(&path, quadrants_jpeg(400, 300)).unwrap();
        let never = Cancel::never();
        // Half is exactly 200 × 150; a quarter of a side less needs the downscale after it; a side
        // past the frame's keeps the frame's size.
        for (side, size) in [(200, (200, 150)), (150, (150, 113)), (1000, (400, 300))] {
            let preview = decode_preview(&path, side, &never).unwrap();
            assert_eq!((preview.width, preview.height), size, "side {side}");
            assert_eq!(
                preview.rgba.len(),
                size.0 as usize * size.1 as usize * 4,
                "side {side}"
            );
            for (actual, expected) in quadrant_colours(&preview).into_iter().zip(QUADRANTS) {
                assert!(
                    actual
                        .iter()
                        .zip(expected)
                        .all(|(actual, expected)| actual.abs_diff(expected) <= 6),
                    "side {side}: {actual:?} against {expected:?}"
                );
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

    /// A missing file, a file that is not a JPEG, one too large to be a cached tier and a cancelled
    /// request each fail, and say why.
    #[test]
    fn preview_display_refuses_what_it_cannot_decode_and_stops_when_cancelled() {
        let dir = temp_dir("preview-display-refused");
        let never = Cancel::never();
        let missing = decode_preview(&dir.join("gone.jpg"), 100, &never).unwrap_err();
        assert_eq!(missing.kind, ErrorKind::FileAccess);
        let text = dir.join("text.jpg");
        fs::write(&text, b"not a jpeg").unwrap();
        assert!(decode_preview(&text, 100, &never).is_err());
        let large = dir.join("large.jpg");
        fs::write(&large, vec![0; MAX_PREVIEW_FILE_BYTES as usize + 1]).unwrap();
        assert_eq!(
            decode_preview(&large, 100, &never).unwrap_err().kind,
            ErrorKind::ResourceLimit
        );
        let path = dir.join("grid.jpg");
        fs::write(&path, quadrants_jpeg(64, 48)).unwrap();
        let cancel = Cancel::new();
        cancel.cancel();
        assert_eq!(
            decode_preview(&path, 32, &cancel).unwrap_err().kind,
            ErrorKind::Cancelled
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
