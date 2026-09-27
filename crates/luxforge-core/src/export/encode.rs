//! Baseline quality-90 JPEG with an embedded sRGB ICC profile.
//!
//! Contract (`docs/design/export.md#behavior`, step 3):
//! - `encode_jpeg(out, frame, exif, progress, cancel)` encodes the RGBA8 sRGB `frame` (alpha is
//!   ignored; it is always 255) at [`super::QUALITY`], writes the JFIF header, the optional EXIF
//!   APP1 payload (`exif` excludes the `Exif\0\0` header) and the ICC profile, and streams the
//!   output into `out` without building the whole file in memory. `progress` receives the encoded
//!   fraction in `0..=1`, at most about once per 1% of rows; `cancel` is checked about as often, and
//!   a cancelled encode returns its error and writes nothing more.
//! - `srgb_profile()` is the one embedded profile, which `crate::profile::check` accepts.

use crate::{Error, Raster};
use image::{GenericImageView, ImageEncoder, ImageError, Rgb, codecs::jpeg::JpegEncoder};
use std::{
    cell::{Cell, RefCell},
    io::{self, BufWriter, Write},
    sync::OnceLock,
};

/// Wraps the caller's writer so a cancellation recorded by [`FrameView`] fails the next write with
/// an I/O error, since `GenericImageView::get_pixel` itself cannot return one.
struct CancelableWriter<'a, W> {
    inner: W,
    cancelled: &'a Cell<bool>,
}

impl<'a, W: Write> Write for CancelableWriter<'a, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.cancelled.get() {
            return Err(io::Error::other("export encode cancelled"));
        }
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A `GenericImageView` over the rendered frame's RGBA bytes read in place: `get_pixel` drops
/// alpha, reports progress by the highest row read (throttled to about 1% steps, always reporting
/// the last row) and polls `cancel` once per row, recording a cancellation for the writer to act on.
struct FrameView<'a> {
    frame: &'a Raster,
    rows_done: Cell<u32>,
    last_reported: Cell<f64>,
    progress: RefCell<&'a mut dyn FnMut(f64)>,
    cancel: &'a dyn Fn() -> Result<(), Error>,
    cancelled: &'a Cell<bool>,
}

impl GenericImageView for FrameView<'_> {
    type Pixel = Rgb<u8>;

    fn dimensions(&self) -> (u32, u32) {
        (self.frame.width, self.frame.height)
    }

    fn get_pixel(&self, x: u32, y: u32) -> Self::Pixel {
        let rows_done = y.saturating_add(1);
        if rows_done > self.rows_done.get() {
            self.rows_done.set(rows_done);
            let height = f64::from(self.frame.height.max(1));
            let fraction = (f64::from(rows_done) / height).min(1.0);
            if fraction - self.last_reported.get() >= 0.01 || rows_done >= self.frame.height {
                self.last_reported.set(fraction);
                (self.progress.borrow_mut())(fraction);
            }
            if !self.cancelled.get() && (self.cancel)().is_err() {
                self.cancelled.set(true);
            }
        }
        let [r, g, b, _a] = self.frame.pixel(x, y).unwrap_or([0, 0, 0, 255]);
        Rgb([r, g, b])
    }
}

pub fn encode_jpeg<W: Write>(
    out: W,
    frame: &Raster,
    exif: Option<&[u8]>,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> Result<(), Error>,
) -> Result<(), Error> {
    let cancelled = Cell::new(false);
    let writer = CancelableWriter {
        inner: BufWriter::new(out),
        cancelled: &cancelled,
    };
    let mut encoder = JpegEncoder::new_with_quality(writer, super::QUALITY);
    encoder
        .set_icc_profile(srgb_profile().to_vec())
        .map_err(|error| Error::internal(format!("jpeg icc profile: {error}")))?;
    if let Some(exif) = exif {
        encoder
            .set_exif_metadata(exif.to_vec())
            .map_err(|error| Error::internal(format!("jpeg exif metadata: {error}")))?;
    }
    let view = FrameView {
        frame,
        rows_done: Cell::new(0),
        last_reported: Cell::new(0.0),
        progress: RefCell::new(progress),
        cancel,
        cancelled: &cancelled,
    };
    match encoder.encode_image(&view) {
        Ok(()) => Ok(()),
        Err(_) if cancelled.get() => Err(Error::cancelled("export encode cancelled")),
        Err(ImageError::IoError(io_error)) => Err(Error::file_access(io_error.to_string())),
        Err(other) => Err(Error::render(format!("jpeg encode: {other}"))),
    }
}

/// The one embedded sRGB ICC profile, built once from `moxcms`'s own sRGB definition (which
/// carries its `ProfileDescription` tag) and encoded through its writer.
pub fn srgb_profile() -> &'static [u8] {
    static PROFILE: OnceLock<Vec<u8>> = OnceLock::new();
    PROFILE
        .get_or_init(|| {
            moxcms::ColorProfile::new_srgb()
                .encode()
                .expect("the built-in sRGB profile always encodes")
        })
        .as_slice()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SnapshotId;
    use image::{ImageDecoder, codecs::jpeg::JpegDecoder};
    use std::{cell::Cell, io::Cursor};

    /// A synthetic frame with gradients and hard edges, sized so neither dimension is a multiple
    /// of 8: `width` and `height` cross block boundaries mid-block, exercising the encoder's
    /// edge-clamping path.
    fn gradient_frame(width: u32, height: u32) -> Raster {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let r = ((x * 255) / width.max(1)) as u8;
                let g = ((y * 255) / height.max(1)) as u8;
                // A hard edge: the right half inverts blue, so the encoder crosses a sharp
                // transition inside some 8x8 blocks.
                let b = if x * 2 < width { 32 } else { 224 };
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        Raster {
            width,
            height,
            rgba: rgba.into(),
            source_fingerprint: "test".to_string(),
            snapshot_id: SnapshotId::default(),
        }
    }

    fn encode(frame: &Raster, exif: Option<&[u8]>) -> (Vec<u8>, Vec<f64>) {
        let mut out = Vec::new();
        let mut progress_calls = Vec::new();
        encode_jpeg(
            &mut out,
            frame,
            exif,
            &mut |fraction| progress_calls.push(fraction),
            &|| Ok(()),
        )
        .expect("encode succeeds");
        (out, progress_calls)
    }

    /// Walks the top-level JPEG segments and returns the markers present, in order, as `(marker,
    /// app_tag)`: `app_tag` is the leading identifier of an APPn payload (e.g. `b"JFIF\0"`), or
    /// empty for a non-APPn marker.
    fn markers(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        assert_eq!(&bytes[0..2], &[0xff, 0xd8], "SOI");
        let mut found = vec![(0xd8u8, Vec::new())];
        let mut i = 2;
        loop {
            assert_eq!(bytes[i], 0xff, "marker prefix at {i}");
            let marker = bytes[i + 1];
            i += 2;
            if marker == 0xd9 {
                found.push((marker, Vec::new()));
                break;
            }
            let size = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
            let payload = &bytes[i + 2..i + size];
            let tag = if (0xe0..=0xef).contains(&marker) {
                let end = payload
                    .iter()
                    .position(|&b| b == 0)
                    .map(|p| p + 1)
                    .unwrap_or(payload.len().min(5));
                payload[..end.min(payload.len())].to_vec()
            } else {
                Vec::new()
            };
            found.push((marker, tag));
            i += size;
            if marker == 0xda {
                // Scan data follows; stop parsing top-level segments here.
                break;
            }
        }
        found
    }

    #[test]
    fn markers_without_metadata() {
        let frame = gradient_frame(257, 131);
        let (bytes, _) = encode(&frame, None);
        let found = markers(&bytes);
        let markers_only: Vec<u8> = found.iter().map(|(m, _)| *m).collect();
        assert_eq!(markers_only[0], 0xd8, "SOI first");
        assert!(markers_only.contains(&0xda), "SOS present");
        // Every APPn marker present is exactly APP0 (JFIF) and APP2 (ICC_PROFILE); no APP1.
        let app_markers: Vec<&(u8, Vec<u8>)> = found
            .iter()
            .filter(|(m, _)| (0xe0..=0xef).contains(m))
            .collect();
        assert_eq!(app_markers.len(), 2, "APP0 and APP2 only: {app_markers:?}");
        assert_eq!(app_markers[0].0, 0xe0, "APP0 first");
        assert!(
            app_markers[0].1.starts_with(b"JFIF\0"),
            "{:?}",
            app_markers[0].1
        );
        assert_eq!(app_markers[1].0, 0xe2, "APP2 second");
        assert!(
            app_markers[1].1.starts_with(b"ICC_PROFILE\0"),
            "{:?}",
            app_markers[1].1
        );
    }

    #[test]
    fn markers_with_metadata_add_exif_between_jfif_and_icc() {
        let frame = gradient_frame(64, 64);
        let exif = vec![0x4d, 0x4du8, 0, 42]; // a minimal, arbitrary TIFF-looking payload
        let (bytes, _) = encode(&frame, Some(&exif));
        let found = markers(&bytes);
        let app_markers: Vec<&(u8, Vec<u8>)> = found
            .iter()
            .filter(|(m, _)| (0xe0..=0xef).contains(m))
            .collect();
        assert_eq!(app_markers.len(), 3, "APP0, APP1, APP2: {app_markers:?}");
        assert_eq!(app_markers[0].0, 0xe0);
        assert_eq!(app_markers[1].0, 0xe1, "APP1 exif between JFIF and ICC");
        assert!(
            app_markers[1].1.starts_with(b"Exif\0"),
            "{:?}",
            app_markers[1].1
        );
        assert_eq!(app_markers[2].0, 0xe2);
    }

    #[test]
    fn chroma_sampling_is_4_4_4() {
        // The design proposal claims full-resolution (4:4:4) chroma; confirm it from the written
        // SOF0 sampling factors rather than trusting the encoder's own doc comment, which claims
        // 4:2:2 despite writing every component at h=1,v=1.
        let frame = gradient_frame(64, 64);
        let (bytes, _) = encode(&frame, None);
        let mut i = 2;
        loop {
            let marker = bytes[i + 1];
            i += 2;
            let size = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
            if marker == 0xc0 {
                // SOF0 payload: precision(1) height(2) width(2) num_components(1) then
                // (id, hv, tq) per component.
                let payload = &bytes[i + 2..i + size];
                let num_components = payload[5] as usize;
                assert_eq!(num_components, 3);
                for c in 0..num_components {
                    let hv = payload[6 + c * 3 + 1];
                    assert_eq!(hv, 0x11, "component {c} is not sampled at 4:4:4 (h=1,v=1)");
                }
                break;
            }
            i += size;
            assert!(marker != 0xda, "SOF0 not found before scan start");
        }
    }

    #[test]
    fn decodes_to_the_same_dimensions_within_tolerance() {
        // Measured on the generated gradient/edge pattern at quality 90, 4:4:4: mean abs error
        // ~0.53/channel and max abs error 8 for 257x131, ~0.33/1 for the 1x1 case. Bounds are
        // stated generously above that so unrelated encoder changes do not make this test flaky.
        const MEAN_ABS_TOLERANCE: f64 = 1.5;
        const MAX_ABS_TOLERANCE: i32 = 16;

        for (width, height) in [(257u32, 131u32), (1, 1)] {
            let frame = gradient_frame(width, height);
            let (bytes, _) = encode(&frame, None);
            let decoder = JpegDecoder::new(Cursor::new(bytes)).expect("valid jpeg");
            assert_eq!(decoder.dimensions(), (width, height));
            let mut decoded = vec![0u8; decoder.total_bytes() as usize];
            decoder.read_image(&mut decoded).expect("decode");

            let mut sum_abs = 0f64;
            let mut max_abs = 0i32;
            let mut count = 0u64;
            for y in 0..height {
                for x in 0..width {
                    let [r, g, b, _a] = frame.pixel(x, y).unwrap();
                    let offset = ((y * width + x) * 3) as usize;
                    let (dr, dg, db) = (
                        decoded[offset] as i32,
                        decoded[offset + 1] as i32,
                        decoded[offset + 2] as i32,
                    );
                    for (original, decoded) in [(r as i32, dr), (g as i32, dg), (b as i32, db)] {
                        let diff = (original - decoded).abs();
                        sum_abs += diff as f64;
                        max_abs = max_abs.max(diff);
                        count += 1;
                    }
                }
            }
            let mean_abs = sum_abs / count as f64;
            assert!(
                mean_abs <= MEAN_ABS_TOLERANCE,
                "{width}x{height}: mean abs error {mean_abs} exceeds {MEAN_ABS_TOLERANCE}"
            );
            assert!(
                max_abs <= MAX_ABS_TOLERANCE,
                "{width}x{height}: max abs error {max_abs} exceeds {MAX_ABS_TOLERANCE}"
            );
        }
    }

    #[test]
    fn no_exif_segment_when_none() {
        let frame = gradient_frame(32, 32);
        let (bytes, _) = encode(&frame, None);
        let mut decoder = JpegDecoder::new(Cursor::new(bytes)).expect("valid jpeg");
        assert_eq!(decoder.exif_metadata().expect("reads exif"), None);
    }

    #[test]
    fn exif_segment_round_trips_the_given_payload() {
        let frame = gradient_frame(32, 32);
        let exif = vec![0x4du8, 0x4d, 0, 42, 1, 2, 3, 4];
        let (bytes, _) = encode(&frame, Some(&exif));
        let mut decoder = JpegDecoder::new(Cursor::new(bytes)).expect("valid jpeg");
        assert_eq!(decoder.exif_metadata().expect("reads exif"), Some(exif));
    }

    #[test]
    fn icc_profile_round_trips_exactly() {
        let frame = gradient_frame(32, 32);
        let (bytes, _) = encode(&frame, None);
        let mut decoder = JpegDecoder::new(Cursor::new(bytes)).expect("valid jpeg");
        let decoded_profile = decoder
            .icc_profile()
            .expect("reads icc")
            .expect("icc present");
        assert_eq!(decoded_profile, srgb_profile());
        crate::profile::check(&decoded_profile, 3).expect("accepted as standard sRGB");
    }

    #[test]
    fn progress_is_monotonic_and_ends_at_one() {
        let frame = gradient_frame(257, 131);
        let (_, progress) = encode(&frame, None);
        assert!(!progress.is_empty());
        let mut previous = 0.0;
        for &fraction in &progress {
            assert!(
                fraction >= previous,
                "progress went backwards: {progress:?}"
            );
            assert!((0.0..=1.0).contains(&fraction));
            previous = fraction;
        }
        assert_eq!(*progress.last().unwrap(), 1.0);
    }

    #[test]
    fn cancel_stops_the_encode_and_writes_nothing_more() {
        let frame = gradient_frame(257, 131);
        let mut out = Vec::new();
        let calls = Cell::new(0u32);
        let result = encode_jpeg(&mut out, &frame, None, &mut |_| {}, &|| {
            calls.set(calls.get() + 1);
            if calls.get() > 2 {
                Err(Error::cancelled("stop"))
            } else {
                Ok(())
            }
        });
        assert_eq!(result.unwrap_err().kind, crate::ErrorKind::Cancelled);

        let (full, _) = encode(&frame, None);
        assert!(
            out.len() < full.len(),
            "cancelled output ({} bytes) should be shorter than a full encode ({} bytes): the \
             encode stopped instead of running to completion",
            out.len(),
            full.len()
        );
    }
}
