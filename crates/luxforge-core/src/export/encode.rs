//! JPEG with an embedded sRGB ICC profile, written through the one JPEG codec (`luxforge-jpeg`,
//! libjpeg-turbo with mozjpeg's optimisations), at the quality, chroma sampling and Optimise the
//! export's [`JpegOptions`] name.
//!
//! Contract (`docs/design/export.md#behavior`, step 3, and `docs/design/export-settings.md`,
//! encoding):
//! - `encode_jpeg(out, frame, options, exif, pixels_per_inch, progress, cancel)` encodes the RGBA8
//!   sRGB `frame` (alpha is ignored; it is always 255), writes the JFIF header (with
//!   `pixels_per_inch` as its density when given, a unitless 1:1 aspect ratio otherwise), the
//!   optional EXIF APP1 payload (`exif` excludes the `Exif\0\0` header) and the ICC profile, and
//!   streams the output into `out` without building the whole file in memory. `progress` receives
//!   the fraction of the whole encode done in `0..=1`, never decreasing, at most about once per 1%
//!   and at its end; `cancel` is checked before every strip of 16 rows and, for an optimised
//!   encode, between and within its passes, and a cancelled encode returns its error and writes
//!   nothing more.
//! - `encode_jpeg_rows(out, (width, height), bands, options, exif, pixels_per_inch, progress,
//!   cancel)` is the same encode for a frame that arrives in bands of whole rows, in order from the
//!   top, pulled one at a time: the same bytes, progress and checks whatever the bands, and
//!   `encode_jpeg` is it given the whole frame as one band. A band the source fails to produce
//!   stops the encode with its error.
//! - Without Optimise the file is baseline (SOF0), one interleaved scan with the standard Huffman
//!   tables, written as the rows arrive: libjpeg's own fastest settings, accepted in
//!   `docs/design/export.md#decisions`. With Optimise it is progressive (SOF2) with optimised
//!   Huffman tables and trellis quantisation, over the same standard quantisation tables, so a
//!   quality means the same tables either way; the encode holds the output's coefficients,
//!   [`encoder_bytes`], at every size, and writes the file once every row has arrived. Chroma is
//!   full (4:4:4) or half (4:2:0).
//! - `encode_jpeg_limited` encodes a held frame in memory under a byte limit, through the
//!   format-agnostic search in [`super::limit`].
//! - libjpeg's failures come back from the codec as errors, never an abort: a writer failure as
//!   `file-access` with its reason, anything else as `render`; an EXIF payload too large for one
//!   APP1 segment is refused before anything is written.
//! - `srgb_profile()` is the one embedded profile, which `crate::profile::check` accepts; the codec
//!   writes it as ICC chunks numbered from 1.

use super::limit::{self, LimitProgress, Limited};
use crate::{Error, Raster};
use luxforge_jpeg::{Encoder, Progress, Settings};
use std::{
    io::{self, Write},
    sync::OnceLock,
};

const EXIF_HEADER: &[u8] = b"Exif\0\0";

/// How a JPEG's chroma is sampled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Until the export's settings arrive (`tasks/rendering/export-settings.json`) every export is
// baseline 4:4:4 and none is limited, so only the tests reach this.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) enum Chroma {
    /// 4:4:4: a chroma sample for every pixel.
    Full,
    /// 4:2:0: a chroma sample for every 2 × 2 pixels.
    Half,
}

impl Chroma {
    /// The pixels one chroma sample covers, across and down, as the codec takes them.
    fn pixels(self) -> (u8, u8) {
        match self {
            Self::Full => (1, 1),
            Self::Half => (2, 2),
        }
    }
}

/// How an export's JPEG is encoded: libjpeg's quality from 1 to 100, the chroma sampling, and
/// whether Optimise makes it progressive with optimised tables and trellis quantisation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct JpegOptions {
    pub(crate) quality: u8,
    pub(crate) chroma: Chroma,
    pub(crate) optimise: bool,
}

/// The export's encoding before its settings arrive: quality 90, 4:4:4, baseline, as
/// `docs/design/export.md` accepted it.
pub(crate) const BASELINE_90: JpegOptions = JpegOptions {
    quality: super::QUALITY,
    chroma: Chroma::Full,
    optimise: false,
};

/// The bytes an encode of a `width` × `height` output with `options` holds beyond the bands it is
/// given: an optimised encode's coefficient arrays, about 12 bytes a pixel at 4:4:4 and 6 at
/// 4:2:0, with no cap; nothing that scales with the image for a baseline one. Pure, for
/// `export.plan`.
// Until the export's settings arrive (`tasks/rendering/export-settings.json`) every export is
// baseline 4:4:4 and none is limited, so only the tests reach this.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn encoder_bytes(width: u32, height: u32, options: &JpegOptions) -> u64 {
    if options.optimise {
        luxforge_jpeg::coefficient_bytes(width, height, options.chroma.pixels())
    } else {
        0
    }
}

/// Encode one rendered frame: [`encode_jpeg_rows`] given the whole frame as one band, which the
/// encoder reads in place.
pub(crate) fn encode_jpeg<W: Write>(
    out: W,
    frame: &Raster,
    options: &JpegOptions,
    exif: Option<&[u8]>,
    pixels_per_inch: Option<u16>,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> Result<(), Error>,
) -> Result<(), Error> {
    let whole = std::iter::once(Ok((frame.height, frame.rgba.as_slice())));
    encode_jpeg_rows(
        out,
        (frame.width, frame.height),
        whole,
        options,
        exif,
        pixels_per_inch,
        progress,
        cancel,
    )
}

/// Encode a `width` × `height` frame that arrives in bands: `bands` yields its rows in order from
/// the top, each band `(rows, rgba)` holding `rows` whole rows of RGBA8, and is pulled one band at
/// a time as the encoder takes them. The encoder hands libjpeg the same 16-row strips whatever the
/// bands, so the bytes are the whole frame's, and before each strip it checks `cancel` and reports
/// the fraction done to `progress`, at most about once per 1%; an optimised encode then runs its
/// passes, checking `cancel` and reporting as they go. An error the source yields stops the encode
/// and is returned as it is; a band that is not its rows, rows past the frame and a source that
/// ends before the last row are refused as `internal`. However the encode stops, what libjpeg
/// still buffers is discarded and `out` receives nothing more.
#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_jpeg_rows<W: Write, B: AsRef<[u8]>>(
    out: W,
    (width, height): (u32, u32),
    bands: impl IntoIterator<Item = Result<(u32, B), Error>>,
    options: &JpegOptions,
    exif: Option<&[u8]>,
    pixels_per_inch: Option<u16>,
    progress: &mut dyn FnMut(f64),
    cancel: &dyn Fn() -> Result<(), Error>,
) -> Result<(), Error> {
    let app1 = exif.map(|exif| [EXIF_HEADER, exif].concat());
    let segments: Vec<(u8, &[u8])> = app1.iter().map(|app1| (1, app1.as_slice())).collect();
    let settings = Settings {
        quality: options.quality,
        chroma: options.chroma.pixels(),
        optimise: options.optimise,
        pixels_per_inch,
        segments: &segments,
        icc: Some(srgb_profile()),
    };
    let mut last_reported = 0.0;
    let mut step = |at: Progress| {
        if cancel().is_err() {
            return Err(Error::cancelled("export encode cancelled"));
        }
        if (at.rows > 0 || at.pass > 0)
            && (at.fraction - last_reported >= 0.01 || at.fraction == 1.0)
        {
            last_reported = at.fraction;
            progress(at.fraction);
        }
        Ok(())
    };
    // Dropped on any error, the session is abandoned as `Encoder::abort` abandons it.
    let mut encoder = Encoder::start(out, width, height, &settings)?;
    for band in bands {
        let (count, rgba) = band?;
        encoder.write_rows(count as usize, rgba.as_ref(), &mut step)?;
    }
    encoder.finish_with(&mut step)
}

/// A writer into memory that refuses the write that would take it past `limit` bytes, so an
/// attempt that cannot fit stops as soon as it passes the limit and never holds more than it.
// Until the export's settings arrive (`tasks/rendering/export-settings.json`) every export is
// baseline 4:4:4 and none is limited, so only the tests reach this.
#[cfg_attr(not(test), allow(dead_code))]
struct Capped {
    bytes: Vec<u8>,
    limit: usize,
    passed: bool,
}

impl Write for Capped {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.bytes.len() + buf.len() > self.limit {
            self.passed = true;
            return Err(io::Error::other("the file passed its size limit"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Encode the held `frame` into memory as the highest quality, at most `options.quality`, whose
/// file is at most `limit_bytes`, by [`limit::search`]: at most eight whole encodes, one after
/// another, each stopped as soon as its file passes the limit and its encoder released before the
/// next starts. `progress` hears each attempt and its encode; `cancel` is checked between attempts
/// and inside each encode. When nothing fits at quality 10 the refusal is `limit-unreachable` and
/// nothing is answered to write.
// Until the export's settings arrive (`tasks/rendering/export-settings.json`) every export is
// baseline 4:4:4 and none is limited, so only the tests reach this.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn encode_jpeg_limited(
    frame: &Raster,
    options: &JpegOptions,
    limit_bytes: u64,
    exif: Option<&[u8]>,
    pixels_per_inch: Option<u16>,
    progress: &mut dyn FnMut(LimitProgress),
    cancel: &dyn Fn() -> Result<(), Error>,
) -> Result<Limited, Error> {
    let limit = usize::try_from(limit_bytes).unwrap_or(usize::MAX);
    let mut attempt = |quality: u8, fraction: &mut dyn FnMut(f64)| {
        let mut capped = Capped {
            bytes: Vec::new(),
            limit,
            passed: false,
        };
        let options = JpegOptions {
            quality,
            ..*options
        };
        let encoded = encode_jpeg(
            &mut capped,
            frame,
            &options,
            exif,
            pixels_per_inch,
            fraction,
            cancel,
        );
        match encoded {
            Ok(()) => Ok(Some(capped.bytes)),
            Err(_) if capped.passed => Ok(None),
            Err(error) => Err(error),
        }
    };
    limit::search(limit_bytes, options.quality, &mut attempt, progress, cancel)
}

/// The one embedded sRGB ICC profile, built once from `moxcms`'s own sRGB definition (which
/// carries its `ProfileDescription` tag) and encoded through its writer.
pub(crate) fn srgb_profile() -> &'static [u8] {
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
            &BASELINE_90,
            exif,
            None,
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

    /// Every scan's header (`SOS` payload) in the file, walking past each scan's entropy-coded
    /// data and the table segments between scans to EOI.
    fn scan_headers(bytes: &[u8]) -> Vec<&[u8]> {
        let (headers, mut at) = segments(bytes);
        let mut scans = vec![headers.last().unwrap().1];
        loop {
            while !(bytes[at] == 0xff
                && bytes[at + 1] != 0x00
                && !(0xd0..=0xd7).contains(&bytes[at + 1]))
            {
                at += 1;
            }
            let marker = bytes[at + 1];
            if marker == 0xd9 {
                assert_eq!(at + 2, bytes.len(), "EOI ends the file");
                return scans;
            }
            let size = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
            if marker == 0xda {
                scans.push(&bytes[at + 4..at + 2 + size]);
            }
            at += 2 + size;
        }
    }

    /// Read from the written file for each option, with EXIF and a density:
    /// - Baseline: one baseline frame (SOF0, no other SOFn), one scan carrying all three
    ///   components and nothing but restart-free entropy data after it up to EOI, and libjpeg's
    ///   standard (Annex K) luminance DC table rather than an optimised one.
    /// - Optimise: one progressive frame (SOF2), mozjpeg's nine scans starting with every
    ///   component's DC, and an optimised luminance DC table.
    /// - Chroma: luma sampled 1×1 at 4:4:4 and 2×2 at 4:2:0, chroma 1×1.
    /// - Every option: the same quantisation tables at the same quality, the same JFIF, EXIF and
    ///   ICC segments, and a file the independent decoder reads back within tolerance.
    #[test]
    fn each_option_writes_its_frame_scans_and_tables() {
        let frame = gradient_frame(257, 131);
        let exif = [0x4d, 0x4d, 0, 42, 1, 2, 3, 4];
        let standard_luma_dc_bits = [0u8, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
        let mut first_header: Option<Vec<(u8, Vec<u8>)>> = None;
        for optimise in [false, true] {
            for (chroma, luma) in [(Chroma::Full, 0x11), (Chroma::Half, 0x22)] {
                let what = format!("{chroma:?}, optimise {optimise}");
                let options = JpegOptions {
                    quality: 90,
                    chroma,
                    optimise,
                };
                let mut bytes = Vec::new();
                encode_jpeg(
                    &mut bytes,
                    &frame,
                    &options,
                    Some(&exif),
                    Some(300),
                    &mut |_| {},
                    &|| Ok(()),
                )
                .unwrap();
                let (headers, _) = segments(&bytes);
                let frames: Vec<u8> = headers
                    .iter()
                    .map(|(marker, _)| *marker)
                    .filter(|marker| {
                        (0xc0..=0xcf).contains(marker) && ![0xc4, 0xc8, 0xcc].contains(marker)
                    })
                    .collect();
                let sof_marker = if optimise { 0xc2 } else { 0xc0 };
                assert_eq!(frames, [sof_marker], "{what}: one SOF");
                let (_, sof) = headers
                    .iter()
                    .find(|(marker, _)| *marker == sof_marker)
                    .unwrap();
                // precision(1) height(2) width(2) components(1), then (id, hv, tq) per component.
                assert_eq!(sof[0], 8, "8-bit precision");
                assert_eq!(u16::from_be_bytes([sof[1], sof[2]]), 131);
                assert_eq!(u16::from_be_bytes([sof[3], sof[4]]), 257);
                assert_eq!(sof[5], 3, "three components");
                let sampling: Vec<u8> =
                    (0..3).map(|component| sof[6 + component * 3 + 1]).collect();
                assert_eq!(sampling, [luma, 0x11, 0x11], "{what}");
                let scans = scan_headers(&bytes);
                if optimise {
                    assert_eq!(scans.len(), 9, "{what}: mozjpeg's progressive script");
                    // (components, three selectors, Ss, Se, AhAl): the first scan is every
                    // component's DC.
                    assert_eq!((scans[0][0], scans[0][7], scans[0][8]), (3, 0, 0), "{what}");
                } else {
                    assert_eq!(scans.len(), 1, "{what}: one scan, then EOI");
                    assert_eq!(scans[0][0], 3, "the scan interleaves all three components");
                }
                let (_, dht) = headers.iter().find(|(marker, _)| *marker == 0xc4).unwrap();
                assert_eq!(dht[0], 0x00, "the first table is luminance DC");
                assert_eq!(
                    dht[1..17] == standard_luma_dc_bits,
                    !optimise,
                    "{what}: standard Huffman tables only without Optimise"
                );
                // The APPn segments as written, then the quantisation tables, `(Pq Tq, values)`,
                // whether libjpeg wrote them in one DQT segment or two.
                let mut kept: Vec<(u8, Vec<u8>)> = headers
                    .iter()
                    .filter(|(marker, _)| (0xe0..=0xef).contains(marker))
                    .map(|(marker, payload)| (*marker, payload.to_vec()))
                    .collect();
                let markers: Vec<u8> = kept.iter().map(|(marker, _)| *marker).collect();
                assert_eq!(markers, [0xe0, 0xe1, 0xe2], "{what}");
                for (_, mut dqt) in headers
                    .iter()
                    .filter(|(marker, _)| *marker == 0xdb)
                    .copied()
                {
                    while !dqt.is_empty() {
                        let length = 1 + 64 * (usize::from(dqt[0] >> 4) + 1);
                        kept.push((0xdb, dqt[..length].to_vec()));
                        dqt = &dqt[length..];
                    }
                }
                assert_eq!(kept.len(), 5, "{what}: two quantisation tables");
                match &first_header {
                    Some(first) => assert!(*first == kept, "{what}: the same header and tables"),
                    None => first_header = Some(kept),
                }
                let decoder = JpegDecoder::new(Cursor::new(&bytes)).expect("valid jpeg");
                assert_eq!(decoder.dimensions(), (257, 131));
                let mut decoded = vec![0u8; decoder.total_bytes() as usize];
                decoder.read_image(&mut decoded).expect("decode");
                let (mean_abs, _) = error_against(&frame, &decoded, 3);
                let tolerance = match chroma {
                    Chroma::Full => MEAN_ABS_TOLERANCE,
                    Chroma::Half => 2.0 * MEAN_ABS_TOLERANCE,
                };
                assert!(mean_abs <= tolerance, "{what}: mean abs error {mean_abs}");
            }
        }
    }

    /// An optimised encode reports the whole encode's fraction across its passes, never
    /// decreasing, to 1; a cancel during its passes, after every row has been given, stops it with
    /// the cancel's error and leaves no finished file.
    #[test]
    fn an_optimised_encode_reports_its_passes_and_cancels_between_them() {
        let frame = gradient_frame(257, 131);
        let options = JpegOptions {
            quality: 85,
            chroma: Chroma::Half,
            optimise: true,
        };
        let mut progress = Vec::new();
        let mut whole = Vec::new();
        encode_jpeg(
            &mut whole,
            &frame,
            &options,
            None,
            None,
            &mut |fraction| progress.push(fraction),
            &|| Ok(()),
        )
        .unwrap();
        for pair in progress.windows(2) {
            assert!(pair[1] >= pair[0], "{progress:?}");
        }
        assert_eq!(progress.last(), Some(&1.0));
        let passes = progress
            .iter()
            .filter(|&&fraction| fraction > 0.5 && fraction < 1.0)
            .count();
        assert!(passes > 5, "the passes after the rows report: {progress:?}");

        let checks = Cell::new(0u32);
        let rows_checks = 131_u32.div_ceil(16) + 1;
        let mut out = Vec::new();
        let error = encode_jpeg(&mut out, &frame, &options, None, None, &mut |_| {}, &|| {
            checks.set(checks.get() + 1);
            if checks.get() > rows_checks + 3 {
                Err(Error::cancelled("stop"))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(
            checks.get(),
            rows_checks + 4,
            "stopped at the first cancelled check"
        );
        assert!(out.len() < whole.len() && !out.ends_with(&[0xff, 0xd9]));
    }

    /// The memory `export.plan` reports for an encode: the coefficient arrays with Optimise, about
    /// 12 bytes a pixel at 4:4:4 and 6 at 4:2:0, and nothing without it.
    #[test]
    fn the_encoder_holds_coefficients_only_when_optimising() {
        let options = |chroma, optimise| JpegOptions {
            quality: 80,
            chroma,
            optimise,
        };
        assert_eq!(encoder_bytes(6000, 4000, &options(Chroma::Full, false)), 0);
        assert_eq!(encoder_bytes(6000, 4000, &options(Chroma::Half, false)), 0);
        assert_eq!(
            encoder_bytes(6000, 4000, &options(Chroma::Full, true)),
            12 * 24_000_000
        );
        assert_eq!(
            encoder_bytes(6000, 4000, &options(Chroma::Half, true)),
            6 * 24_000_000
        );
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
        let error = encode_jpeg(
            &mut out,
            &frame,
            &BASELINE_90,
            Some(&exif),
            None,
            &mut |_| {},
            &|| Ok(()),
        )
        .unwrap_err();
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
        let result = encode_jpeg(
            &mut out,
            &frame,
            &BASELINE_90,
            None,
            None,
            &mut |_| {},
            &|| {
                calls.set(calls.get() + 1);
                if calls.get() > 2 {
                    Err(Error::cancelled("stop"))
                } else {
                    Ok(())
                }
            },
        );
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

    /// `frame` in bands of `size` rows, the last cut short where the frame ends, each an owned
    /// buffer, as a renderer that produces bands hands them over.
    fn bands(frame: &Raster, size: usize) -> Vec<Result<(u32, Vec<u8>), Error>> {
        let stride = frame.width as usize * 4;
        frame
            .rgba
            .chunks(size * stride)
            .map(|band| Ok(((band.len() / stride) as u32, band.to_vec())))
            .collect()
    }

    /// Bands of 1, 7, 16 and 296 rows encode the bytes the whole frame does, EXIF and profile
    /// included, with the same progress and the same checks of the cancel: one before each 16-row
    /// strip and one after the last.
    #[test]
    fn bands_encode_the_whole_frames_bytes_with_its_progress_and_checks() {
        let frame = gradient_frame(257, 131);
        let exif = [0x4d, 0x4d, 0, 42, 1, 2, 3, 4];
        let checks = Cell::new(0u32);
        let check = || {
            checks.set(checks.get() + 1);
            Ok(())
        };
        let mut whole = Vec::new();
        let mut whole_progress = Vec::new();
        encode_jpeg(
            &mut whole,
            &frame,
            &BASELINE_90,
            Some(&exif),
            None,
            &mut |fraction| whole_progress.push(fraction),
            &check,
        )
        .unwrap();
        let whole_checks = checks.replace(0);
        assert_eq!(whole_checks, 131_u32.div_ceil(16) + 1);
        for size in [1, 7, 16, 296] {
            let mut out = Vec::new();
            let mut progress = Vec::new();
            encode_jpeg_rows(
                &mut out,
                (frame.width, frame.height),
                bands(&frame, size),
                &BASELINE_90,
                Some(&exif),
                None,
                &mut |fraction| progress.push(fraction),
                &check,
            )
            .unwrap();
            assert!(out == whole, "bands of {size} encode other bytes");
            assert_eq!(progress, whole_progress, "bands of {size}");
            assert_eq!(checks.replace(0), whole_checks, "bands of {size}");
        }
    }

    /// A band the source cannot produce stops the encode with the source's own error; a band that
    /// is not the rows it declares and a source that ends before the last row are refused as
    /// `internal`. None is an abort, and each leaves only the start of the file in the writer.
    #[test]
    fn a_band_source_that_fails_or_falls_short_stops_the_encode() {
        let frame = gradient_frame(257, 131);
        let (whole, _) = encode(&frame, None);
        let mut failing = bands(&frame, 16);
        failing[3] = Err(Error::render("the renderer stopped"));
        let mut declared = bands(&frame, 16);
        if let Ok((rows, _)) = &mut declared[2] {
            *rows += 1;
        }
        let mut short = bands(&frame, 16);
        short.pop();
        for (source, kind, detail) in [
            (failing, ErrorKind::Render, "the renderer stopped"),
            (
                declared,
                ErrorKind::Internal,
                "a band's pixels are not the rows it declares",
            ),
            (short, ErrorKind::Internal, "finished before the last row"),
        ] {
            let mut out = Vec::new();
            let error = encode_jpeg_rows(
                &mut out,
                (frame.width, frame.height),
                source,
                &BASELINE_90,
                None,
                None,
                &mut |_| {},
                &|| Ok(()),
            )
            .unwrap_err();
            assert_eq!(error.kind, kind, "{error:?}");
            assert!(error.detail.contains(detail), "{error:?}");
            assert!(
                out.len() < whole.len() && whole.starts_with(&out),
                "{detail}"
            );
        }
    }

    #[test]
    fn a_libjpeg_error_is_an_error_not_an_abort() {
        // libjpeg refuses an empty image and one wider than its 65,500 px limit with a fatal
        // error, which unwinds out of the C code and must come back as an ordinary error.
        for (width, height) in [(0u32, 0u32), (65_536, 1)] {
            let frame = gradient_frame(width, height);
            let mut out = Vec::new();
            let error = encode_jpeg(
                &mut out,
                &frame,
                &BASELINE_90,
                None,
                None,
                &mut |_| {},
                &|| Ok(()),
            )
            .unwrap_err();
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
        let error = encode_jpeg(Full, &frame, &BASELINE_90, None, None, &mut |_| {}, &|| {
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::FileAccess, "{error:?}");
        assert!(error.detail.contains("the disk is full"), "{error:?}");
    }
}
