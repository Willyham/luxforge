//! Baseline quality-90 JPEG with an embedded sRGB ICC profile, written through the one JPEG codec
//! (`luxforge-jpeg`, libjpeg-turbo).
//!
//! Contract (`docs/design/export.md#behavior`, step 3):
//! - `encode_jpeg(out, frame, exif, progress, cancel)` encodes the RGBA8 sRGB `frame` (alpha is
//!   ignored; it is always 255) at [`super::QUALITY`], writes the JFIF header, the optional EXIF
//!   APP1 payload (`exif` excludes the `Exif\0\0` header) and the ICC profile, and streams the
//!   output into `out` without building the whole file in memory. `progress` receives the encoded
//!   fraction in `0..=1`, at most about once per 1% of rows; `cancel` is checked about as often, and
//!   a cancelled encode returns its error and writes nothing more.
//! - The file is baseline (SOF0) with full-resolution chroma (4:4:4 at every component's 1×1
//!   sampling), one interleaved scan and the standard Huffman tables: libjpeg's own fastest
//!   settings, accepted in `docs/design/export.md#decisions`.
//! - libjpeg's failures come back from the codec as errors, never an abort: a writer failure as
//!   `file-access` with its reason, anything else as `render`; an EXIF payload too large for one
//!   APP1 segment is refused before anything is written.
//! - `srgb_profile()` is the one embedded profile, which `crate::profile::check` accepts; the codec
//!   writes it as ICC chunks numbered from 1.

use crate::{Error, Raster};
use luxforge_jpeg::Settings;
use std::{io::Write, sync::OnceLock};

const EXIF_HEADER: &[u8] = b"Exif\0\0";

pub fn encode_jpeg<W: Write>(
    out: W,
    frame: &Raster,
    exif: Option<&[u8]>,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> Result<(), Error>,
) -> Result<(), Error> {
    let app1 = exif.map(|exif| [EXIF_HEADER, exif].concat());
    let segments: Vec<(u8, &[u8])> = app1.iter().map(|app1| (1, app1.as_slice())).collect();
    let settings = Settings {
        quality: super::QUALITY,
        chroma: (1, 1),
        segments: &segments,
        icc: Some(srgb_profile()),
    };
    let rows = frame.height as usize;
    let mut last_reported = 0.0;
    luxforge_jpeg::encode(
        out,
        frame.width,
        frame.height,
        &frame.rgba,
        &settings,
        &mut |rows_done| {
            if cancel().is_err() {
                return Err(Error::cancelled("export encode cancelled"));
            }
            if rows_done > 0 {
                let fraction = rows_done as f64 / rows as f64;
                if fraction - last_reported >= 0.01 || rows_done == rows {
                    last_reported = fraction;
                    progress(fraction);
                }
            }
            Ok(())
        },
    )
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
    use crate::{ErrorKind, SnapshotId};
    use image::{ImageDecoder, codecs::jpeg::JpegDecoder};
    use luxforge_jpeg::MAX_SEGMENT_PAYLOAD;
    use std::{cell::Cell, io, io::Cursor};

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

    /// The header segments up to and including the first SOS, as `(marker, payload)`, and the
    /// offset where that scan's entropy-coded data begins.
    fn segments(bytes: &[u8]) -> (Vec<(u8, &[u8])>, usize) {
        assert_eq!(&bytes[0..2], &[0xff, 0xd8], "SOI");
        let mut found = Vec::new();
        let mut i = 2;
        loop {
            assert_eq!(bytes[i], 0xff, "marker prefix at {i}");
            let marker = bytes[i + 1];
            let size = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
            found.push((marker, &bytes[i + 4..i + 2 + size]));
            i += 2 + size;
            if marker == 0xda {
                return (found, i);
            }
        }
    }

    /// Walks the top-level JPEG segments and returns the markers present, in order, as `(marker,
    /// app_tag)`: `app_tag` is the leading identifier of an APPn payload (e.g. `b"JFIF\0"`), or
    /// empty for a non-APPn marker.
    fn markers(bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut found = vec![(0xd8u8, Vec::new())];
        for (marker, payload) in segments(bytes).0 {
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
        }
        found
    }

    /// Per-channel mean and maximum absolute error of decoded RGB `pixels` against `frame`.
    fn error_against(frame: &Raster, pixels: &[u8], channels: usize) -> (f64, i32) {
        let mut sum_abs = 0f64;
        let mut max_abs = 0i32;
        let mut count = 0u64;
        for (original, decoded) in frame
            .rgba
            .chunks_exact(4)
            .zip(pixels.chunks_exact(channels))
        {
            for channel in 0..3 {
                let diff = (i32::from(original[channel]) - i32::from(decoded[channel])).abs();
                sum_abs += f64::from(diff);
                max_abs = max_abs.max(diff);
                count += 1;
            }
        }
        (sum_abs / count as f64, max_abs)
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
    fn baseline_4_4_4_in_one_interleaved_scan_with_the_standard_tables() {
        // Read from the written file: a baseline frame (SOF0, no other SOFn) sampling every
        // component at 1×1, one scan carrying all three components and nothing but restart-free
        // entropy data after it up to EOI, and libjpeg's standard (Annex K) luminance DC table
        // rather than an optimized one.
        let frame = gradient_frame(257, 131);
        let (bytes, _) = encode(&frame, None);
        let (headers, scan) = segments(&bytes);
        let frames: Vec<u8> = headers
            .iter()
            .map(|(marker, _)| *marker)
            .filter(|marker| (0xc0..=0xcf).contains(marker) && ![0xc4, 0xc8, 0xcc].contains(marker))
            .collect();
        assert_eq!(frames, [0xc0], "one baseline SOF0");
        let (_, sof) = headers.iter().find(|(marker, _)| *marker == 0xc0).unwrap();
        // precision(1) height(2) width(2) components(1), then (id, hv, tq) per component.
        assert_eq!(sof[0], 8, "8-bit precision");
        assert_eq!(u16::from_be_bytes([sof[1], sof[2]]), 131);
        assert_eq!(u16::from_be_bytes([sof[3], sof[4]]), 257);
        assert_eq!(sof[5], 3, "three components");
        for component in 0..3 {
            assert_eq!(
                sof[6 + component * 3 + 1],
                0x11,
                "component {component} is not sampled at 4:4:4 (h=1,v=1)"
            );
        }
        let (_, sos) = headers.last().unwrap();
        assert_eq!(sos[0], 3, "the scan interleaves all three components");
        // Nothing but stuffed bytes in the entropy data: the next marker is EOI, at the end.
        let data = &bytes[scan..];
        let next_marker = data
            .windows(2)
            .position(|pair| pair[0] == 0xff && pair[1] != 0x00)
            .expect("EOI follows the scan");
        assert_eq!(&data[next_marker..], &[0xff, 0xd9], "one scan, then EOI");
        let standard_luma_dc_bits = [0u8, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
        let (_, dht) = headers.iter().find(|(marker, _)| *marker == 0xc4).unwrap();
        assert_eq!(dht[0], 0x00, "the first table is luminance DC");
        assert_eq!(dht[1..17], standard_luma_dc_bits, "standard Huffman table");
    }

    // Measured on the generated gradient/edge pattern at quality 90, 4:4:4, decoded by `image`'s
    // JPEG decoder: mean abs error ~0.53/channel and max abs error 7 for 257x131, ~0.33/1 for the
    // 1x1 case, ~0.49/6 for 1031x677. Bounds are stated generously above that so unrelated encoder
    // changes do not make these tests flaky.
    const MEAN_ABS_TOLERANCE: f64 = 1.5;
    const MAX_ABS_TOLERANCE: i32 = 16;

    #[test]
    fn decodes_to_the_same_dimensions_within_tolerance() {
        for (width, height) in [(257u32, 131u32), (1, 1)] {
            let frame = gradient_frame(width, height);
            let (bytes, _) = encode(&frame, None);
            let decoder = JpegDecoder::new(Cursor::new(bytes)).expect("valid jpeg");
            assert_eq!(decoder.dimensions(), (width, height));
            let mut decoded = vec![0u8; decoder.total_bytes() as usize];
            decoder.read_image(&mut decoded).expect("decode");
            let (mean_abs, max_abs) = error_against(&frame, &decoded, 3);
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
    fn luxforge_opens_an_export_as_a_source_within_tolerance() {
        // Re-importing an export goes through the source path's own decoder, the same libjpeg
        // that wrote it, with its ICC chunk and its own checks; an export must come back as the
        // frame it encoded, within the bounds the independent decoder meets.
        let frame = gradient_frame(1031, 677);
        let (bytes, _) = encode(&frame, None);
        let source = crate::source::open_source_bytes(bytes).expect("an export opens as a source");
        assert_eq!((source.width, source.height), (1031, 677));
        let (mean_abs, max_abs) = error_against(&frame, &source.rgba, 4);
        assert!(mean_abs <= MEAN_ABS_TOLERANCE, "mean abs error {mean_abs}");
        assert!(max_abs <= MAX_ABS_TOLERANCE, "max abs error {max_abs}");
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
    fn exif_larger_than_one_segment_is_refused_before_writing() {
        let frame = gradient_frame(32, 32);
        let exif = vec![0u8; MAX_SEGMENT_PAYLOAD - EXIF_HEADER.len() + 1];
        let mut out = Vec::new();
        let error =
            encode_jpeg(&mut out, &frame, Some(&exif), &mut |_| {}, &|| Ok(())).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(out.is_empty());
    }

    #[test]
    fn icc_profile_round_trips_exactly() {
        let frame = gradient_frame(32, 32);
        let (bytes, _) = encode(&frame, None);
        // One chunk, numbered 1 of 1 as the ICC specification requires.
        let (headers, _) = segments(&bytes);
        let (_, icc) = headers.iter().find(|(marker, _)| *marker == 0xe2).unwrap();
        assert_eq!(icc[..14], *b"ICC_PROFILE\0\x01\x01");
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
        assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled);
        assert_eq!(
            calls.get(),
            3,
            "the encode stopped at the first cancelled check"
        );

        let (full, _) = encode(&frame, None);
        assert!(
            out.len() < full.len(),
            "cancelled output ({} bytes) should be shorter than a full encode ({} bytes): the \
             encode stopped instead of running to completion",
            out.len(),
            full.len()
        );
    }

    #[test]
    fn a_libjpeg_error_is_an_error_not_an_abort() {
        // libjpeg refuses an empty image and one wider than its 65,500 px limit with a fatal
        // error, which unwinds out of the C code and must come back as an ordinary error.
        for (width, height) in [(0u32, 0u32), (65_536, 1)] {
            let frame = gradient_frame(width, height);
            let mut out = Vec::new();
            let error = encode_jpeg(&mut out, &frame, None, &mut |_| {}, &|| Ok(())).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Render, "{width}x{height}: {error:?}");
            assert!(
                error.detail.contains("libjpeg"),
                "{width}x{height}: {error:?}"
            );
        }
        // The encoder still works afterwards.
        encode(&gradient_frame(8, 8), None);
    }

    #[test]
    fn a_write_failure_is_a_file_access_error() {
        struct Full;
        impl Write for Full {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "the disk is full",
                ))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let frame = gradient_frame(257, 131);
        let error = encode_jpeg(Full, &frame, None, &mut |_| {}, &|| Ok(())).unwrap_err();
        assert_eq!(error.kind, ErrorKind::FileAccess, "{error:?}");
        assert!(error.detail.contains("the disk is full"), "{error:?}");
    }
}
