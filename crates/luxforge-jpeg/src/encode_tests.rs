//! The encode's options: Optimise and chroma sampling, read back by an independent decoder and by
//! walking the written markers here rather than through the crate's own container walk; the
//! coefficient-memory estimate against libjpeg's own pool count; and the progress and the stop of
//! an optimised encode's passes.

use crate::{
    compress::take_peak_pool_bytes,
    tests::{Seeded, Stepped, fixture, scene},
    *,
};
use image::{ImageDecoder, codecs::jpeg::JpegDecoder};
use luxforge_test_macros::slow;
use mozjpeg::{ColorSpace, Compress, Marker, PixelDensity, PixelDensityUnit};
use std::{io::Cursor, time::Instant};

/// One marker segment of a written file: its marker and payload (empty for a marker without one).
#[derive(Debug)]
struct Written<'a> {
    marker: u8,
    payload: &'a [u8],
}

/// Every marker of a file in order, SOI to EOI, stepping over each scan's entropy-coded data (its
/// stuffed `FF 00` bytes and restart markers), written here so that the tests do not read the
/// encoder's output through the crate's own container walk.
fn walk(bytes: &[u8]) -> Vec<Written<'_>> {
    assert_eq!(&bytes[..2], [0xff, 0xd8], "SOI");
    let mut found = vec![Written {
        marker: 0xd8,
        payload: &[],
    }];
    let mut at = 2;
    loop {
        assert_eq!(bytes[at], 0xff, "a marker at {at}");
        let marker = bytes[at + 1];
        if marker == 0xd9 {
            found.push(Written {
                marker,
                payload: &[],
            });
            assert_eq!(at + 2, bytes.len(), "EOI ends the file");
            return found;
        }
        let length = usize::from(u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]));
        found.push(Written {
            marker,
            payload: &bytes[at + 4..at + 2 + length],
        });
        at += 2 + length;
        if marker == 0xda {
            // Entropy-coded data up to the next marker that is neither stuffing nor a restart.
            while !(bytes[at] == 0xff
                && bytes[at + 1] != 0
                && !(0xd0..=0xd7).contains(&bytes[at + 1]))
            {
                at += 1;
            }
        }
    }
}

/// The segments before the first frame or table segment: the JFIF header, the caller's APPn and
/// the ICC chunks, as written.
fn header(found: &[Written<'_>]) -> Vec<(u8, Vec<u8>)> {
    found
        .iter()
        .skip(1)
        .take_while(|segment| (0xe0..=0xef).contains(&segment.marker))
        .map(|segment| (segment.marker, segment.payload.to_vec()))
        .collect()
}

/// The SOF markers a file has.
fn frames(found: &[Written<'_>]) -> Vec<u8> {
    found
        .iter()
        .map(|segment| segment.marker)
        .filter(|marker| (0xc0..=0xcf).contains(marker) && ![0xc4, 0xc8, 0xcc].contains(marker))
        .collect()
}

/// Each component's `(id, sampling, table)` from the frame header.
fn components(found: &[Written<'_>]) -> Vec<(u8, u8, u8)> {
    let sof = found
        .iter()
        .find(|segment| [0xc0, 0xc1, 0xc2].contains(&segment.marker))
        .expect("a frame header")
        .payload;
    assert_eq!(sof[0], 8, "8-bit samples");
    sof[6..]
        .chunks_exact(3)
        .map(|component| (component[0], component[1], component[2]))
        .collect()
}

/// Each scan's `(components, Ss, Se, Ah << 4 | Al)`.
fn scans(found: &[Written<'_>]) -> Vec<(u8, u8, u8, u8)> {
    found
        .iter()
        .filter(|segment| segment.marker == 0xda)
        .map(|segment| {
            let sos = segment.payload;
            let n = usize::from(sos[0]);
            (sos[0], sos[1 + 2 * n], sos[2 + 2 * n], sos[3 + 2 * n])
        })
        .collect()
}

/// The quantisation tables, `(table, 64 values in zigzag order)`, in the order written.
fn quantisation(found: &[Written<'_>]) -> Vec<(u8, Vec<u16>)> {
    let mut tables = Vec::new();
    for segment in found.iter().filter(|segment| segment.marker == 0xdb) {
        let mut rest = segment.payload;
        while !rest.is_empty() {
            let (precision, table) = (rest[0] >> 4, rest[0] & 15);
            let values: Vec<u16> = if precision == 0 {
                rest[1..65].iter().map(|&value| u16::from(value)).collect()
            } else {
                rest[1..129]
                    .chunks_exact(2)
                    .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                    .collect()
            };
            tables.push((table, values));
            rest = &rest[1 + 64 * (usize::from(precision) + 1)..];
        }
    }
    tables
}

/// The Huffman tables, `(class << 4 | table, 16 code-length counts)`, in the order written.
fn huffman(found: &[Written<'_>]) -> Vec<(u8, [u8; 16])> {
    let mut tables = Vec::new();
    for segment in found.iter().filter(|segment| segment.marker == 0xc4) {
        let mut rest = segment.payload;
        while !rest.is_empty() {
            let counts: [u8; 16] = rest[1..17].try_into().unwrap();
            let symbols: usize = counts.iter().map(|&count| usize::from(count)).sum();
            tables.push((rest[0], counts));
            rest = &rest[17 + symbols..];
        }
    }
    tables
}

/// The JPEG standard's (ITU T.81 Annex K) example tables, in natural order: K.1 luminance, K.2
/// chrominance.
const ANNEX_K: [[u16; 64]; 2] = [
    [
        16, 11, 10, 16, 24, 40, 51, 61, 12, 12, 14, 19, 26, 58, 60, 55, 14, 13, 16, 24, 40, 57, 69,
        56, 14, 17, 22, 29, 51, 87, 80, 62, 18, 22, 37, 56, 68, 109, 103, 77, 24, 35, 55, 64, 81,
        104, 113, 92, 49, 64, 78, 87, 103, 121, 120, 101, 72, 92, 95, 98, 112, 100, 103, 99,
    ],
    [
        17, 18, 24, 47, 99, 99, 99, 99, 18, 21, 26, 66, 99, 99, 99, 99, 24, 26, 56, 99, 99, 99, 99,
        99, 47, 66, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
        99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99, 99,
    ],
];

/// The natural-order index of each zigzag position (ITU T.81 Figure A.6).
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Annex K's table `which` scaled to `quality` as the IJG's `jpeg_quality_scaling` and
/// `jpeg_add_quant_table` do (without forcing baseline), in zigzag order.
fn annex_k(which: usize, quality: u8) -> Vec<u16> {
    let quality = u32::from(quality.clamp(1, 100));
    let scale = if quality < 50 {
        5000 / quality
    } else {
        200 - quality * 2
    };
    ZIGZAG
        .iter()
        .map(|&natural| {
            ((u32::from(ANNEX_K[which][natural]) * scale + 50) / 100).clamp(1, 32767) as u16
        })
        .collect()
}

/// The ITU T.81 Annex K.3 standard Huffman tables' code-length counts in the order libjpeg writes
/// them for an interleaved scan: luminance DC and AC, then chrominance DC and AC.
const STANDARD_HUFFMAN: [(u8, [u8; 16]); 4] = [
    (0x00, [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0]),
    (0x10, [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d]),
    (0x01, [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0]),
    (0x11, [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77]),
];

fn settings<'a>(
    quality: u8,
    chroma: (u8, u8),
    optimise: bool,
    segments: &'a [(u8, &'a [u8])],
    icc: Option<&'a [u8]>,
    pixels_per_inch: Option<u16>,
) -> Settings<'a> {
    Settings {
        quality,
        chroma,
        optimise,
        pixels_per_inch,
        segments,
        icc,
    }
}

/// Encode `rgba` whole, answering the file and every progress report.
fn encoded(
    width: u32,
    height: u32,
    rgba: &[u8],
    settings: &Settings<'_>,
) -> (Vec<u8>, Vec<Progress>) {
    let mut out = Vec::new();
    let mut reports = Vec::new();
    encode(&mut out, width, height, rgba, settings, &mut |progress| {
        reports.push(progress);
        Ok::<_, JpegError>(())
    })
    .unwrap();
    (out, reports)
}

/// The independent decoder's (`image`'s, zune-jpeg) RGB pixels of `bytes`, checked to be
/// `width` × `height`.
fn independent(bytes: &[u8], width: u32, height: u32) -> Vec<u8> {
    let decoder = JpegDecoder::new(Cursor::new(bytes)).expect("the independent decoder reads it");
    assert_eq!(decoder.dimensions(), (width, height));
    let mut rgb = vec![0; decoder.total_bytes() as usize];
    decoder.read_image(&mut rgb).unwrap();
    rgb
}

/// The mean absolute difference per channel between `rgba` and decoded RGB `rgb`.
fn mean_error(rgba: &[u8], rgb: &[u8]) -> f64 {
    let total: u64 = rgba
        .chunks_exact(4)
        .zip(rgb.chunks_exact(3))
        .flat_map(|(a, b)| (0..3).map(move |channel| u64::from(a[channel].abs_diff(b[channel]))))
        .sum();
    total as f64 / (rgb.len() as f64)
}

/// The encode as the crate wrote it before Optimise, through `mozjpeg`'s safe API: its fastest
/// (libjpeg-turbo) profile, the quality and sampling, the density, the caller's segments and the
/// ICC chunks after the JFIF header, then the rows in strips.
fn through_the_safe_api(width: u32, height: u32, rgba: &[u8], settings: &Settings<'_>) -> Vec<u8> {
    let mut compress = Compress::new(ColorSpace::JCS_EXT_RGBA);
    compress.set_fastest_defaults();
    compress.set_size(width as usize, height as usize);
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
    let mut started = compress.start_compress(Vec::new()).unwrap();
    for (n, payload) in settings.segments {
        started.write_marker(Marker::APP(*n), payload);
    }
    if let Some(profile) = settings.icc {
        let chunks = profile
            .chunks(crate::icc::MAX_CHUNK_DATA)
            .collect::<Vec<_>>();
        for (index, chunk) in chunks.iter().enumerate() {
            let payload = [
                b"ICC_PROFILE\0".as_slice(),
                &[index as u8 + 1, chunks.len() as u8],
                chunk,
            ]
            .concat();
            started.write_marker(Marker::APP(2), &payload);
        }
    }
    for strip in rgba.chunks(STRIP_ROWS * width as usize * 4) {
        started.write_scanlines(strip).unwrap();
    }
    started.finish().unwrap()
}

/// With Optimise off, the encode writes byte for byte what an encode through `mozjpeg`'s safe API
/// writes: for 4:4:4 and 4:2:0, a high and a low quality (where the tables pass 255 and the frame
/// becomes extended sequential), a density, an EXIF segment and an ICC profile, and edges that are
/// no multiple of a block.
#[test]
fn optimise_off_writes_the_bytes_the_safe_api_wrote() {
    let profile: Vec<u8> = (0..70_000).map(|i| (i % 249) as u8).collect();
    let exif = b"Exif\0\0MM\0*\0\0\0\x08".as_slice();
    let segments = [(1, exif)];
    for (width, height) in [(61, 47), (300, 200)] {
        let rgba = scene(width, height);
        for chroma in [(1, 1), (2, 2)] {
            for (quality, with_metadata) in [(90, true), (75, false), (12, false), (100, true)] {
                let settings = if with_metadata {
                    settings(quality, chroma, false, &segments, Some(&profile), Some(300))
                } else {
                    settings(quality, chroma, false, &[], None, None)
                };
                let (ours, _) = encoded(width, height, &rgba, &settings);
                let theirs = through_the_safe_api(width, height, &rgba, &settings);
                assert!(
                    ours == theirs,
                    "{width}x{height} at {chroma:?}, quality {quality}: {} and {} bytes",
                    ours.len(),
                    theirs.len()
                );
            }
        }
    }
}

/// What each option writes, read from the markers and decoded by an independent decoder:
/// - Off: one baseline frame (SOF0), one interleaved scan of the whole spectrum, and the four
///   standard Huffman tables.
/// - On: one progressive frame (SOF2), several scans starting with the DC coefficients, and
///   Huffman tables optimised for the image, none of them the standard luminance AC table.
/// - Both: the sampling asked for (luma 1×1 or 2×2, chroma 1×1), the same standard (Annex K)
///   quantisation tables scaled to the quality, and the same JFIF density, APPn and ICC segments.
#[test]
fn each_option_writes_its_frame_scans_and_tables() {
    let (width, height) = (203, 151);
    let rgba = scene(width, height);
    let profile: Vec<u8> = (0..3000).map(|i| (i % 251) as u8).collect();
    let exif = b"Exif\0\0MM\0*\0\0\0\x08".as_slice();
    let segments = [(1, exif), (13, b"Photoshop 3.0\0".as_slice())];
    for quality in [90, 40] {
        for (chroma, luma_sampling) in [((1, 1), 0x11), ((2, 2), 0x22)] {
            let mut headers = Vec::new();
            let mut errors = Vec::new();
            for optimise in [false, true] {
                let what = format!("quality {quality}, {chroma:?}, optimise {optimise}");
                let settings = settings(
                    quality,
                    chroma,
                    optimise,
                    &segments,
                    Some(&profile),
                    Some(240),
                );
                let (bytes, _) = encoded(width, height, &rgba, &settings);
                let found = walk(&bytes);
                assert_eq!(
                    frames(&found),
                    [if optimise { 0xc2 } else { 0xc0 }],
                    "{what}"
                );
                assert_eq!(
                    components(&found),
                    [(1, luma_sampling, 0), (2, 0x11, 1), (3, 0x11, 1)],
                    "{what}"
                );
                assert_eq!(
                    quantisation(&found),
                    [(0, annex_k(0, quality)), (1, annex_k(1, quality))],
                    "{what}"
                );
                let scans = scans(&found);
                let tables = huffman(&found);
                if optimise {
                    // mozjpeg's script (`jpeg_scan_rgb.txt` from jpgcrush): every component's DC,
                    // then each component's low and high frequencies, luma's in three bits.
                    assert_eq!(
                        scans,
                        [
                            (3, 0, 0, 0x00),
                            (1, 1, 8, 0x02),
                            (1, 1, 8, 0x00),
                            (1, 1, 8, 0x00),
                            (1, 9, 63, 0x02),
                            (1, 1, 63, 0x21),
                            (1, 1, 63, 0x10),
                            (1, 9, 63, 0x00),
                            (1, 9, 63, 0x00),
                        ],
                        "{what}"
                    );
                    assert!(
                        !tables.contains(&STANDARD_HUFFMAN[1]),
                        "{what}: optimised tables, {tables:?}"
                    );
                } else {
                    assert_eq!(scans, [(3, 0, 63, 0)], "{what}");
                    assert_eq!(tables, STANDARD_HUFFMAN, "{what}");
                }
                let rgb = independent(&bytes, width, height);
                errors.push(mean_error(&rgba, &rgb));
                headers.push(header(&found));
            }
            // The scene's grain is fine texture that quantisation removes; trellis quantisation
            // trades a little more of it for size.
            assert!(errors[0] < 16.0, "{errors:?}");
            assert!(errors[1] < errors[0] * 1.25, "{errors:?}");
            assert_eq!(
                headers[0], headers[1],
                "the same header segments either way"
            );
            let markers: Vec<u8> = headers[0].iter().map(|(marker, _)| *marker).collect();
            assert_eq!(markers, [0xe0, 0xe1, 0xed, 0xe2]);
            assert_eq!(
                &headers[0][0].1[7..12],
                [1, 0, 240, 0, 240],
                "density in inches"
            );
        }
    }
}

/// libjpeg's pool count while an optimised encode holds its coefficient arrays, less the same
/// count for the baseline encode, is the estimate within 1%: the arrays dominate what Optimise
/// adds (the rest is its entropy statistics and scan buffers in libjpeg's small pools, a few
/// KiB). Edges that are no multiple of a block or an MCU are padded as libjpeg pads them.
#[test]
#[slow]
fn the_coefficient_estimate_is_what_libjpeg_allocates() {
    for (width, height) in [(1024, 768), (1001, 667)] {
        let rgba = scene(width, height);
        for chroma in [(1, 1), (2, 2)] {
            let mut peaks = [0; 2];
            for (peak, optimise) in peaks.iter_mut().zip([false, true]) {
                take_peak_pool_bytes();
                encoded(
                    width,
                    height,
                    &rgba,
                    &settings(85, chroma, optimise, &[], None, None),
                );
                *peak = take_peak_pool_bytes() as i64;
            }
            let estimate = coefficient_bytes(width, height, chroma) as i64;
            let added = peaks[1] - peaks[0];
            println!(
                "{width}x{height} {chroma:?}: estimate {estimate} ({:.3} B/px), libjpeg adds {added} ({:.3} B/px)",
                estimate as f64 / f64::from(width * height),
                added as f64 / f64::from(width * height)
            );
            assert!(
                (added - estimate).abs() <= estimate / 100,
                "{width}x{height} {chroma:?}: estimate {estimate}, libjpeg added {added}"
            );
        }
    }
}

/// The estimate is 12 bytes a pixel at 4:4:4 and 6 at 4:2:0 where nothing pads, and pads as
/// libjpeg does: a 4:2:0 image's luma to whole 16-pixel MCUs.
#[test]
fn the_coefficient_estimate_counts_padded_blocks() {
    assert_eq!(coefficient_bytes(64, 32, (1, 1)), 12 * 64 * 32);
    assert_eq!(coefficient_bytes(64, 32, (2, 2)), 6 * 64 * 32);
    // 1×1: one block a component at 4:4:4; at 4:2:0 a 2×2-block luma MCU and a chroma block each.
    assert_eq!(coefficient_bytes(1, 1, (1, 1)), 2 * 128 * 3);
    assert_eq!(coefficient_bytes(1, 1, (2, 2)), 2 * 128 * (4 + 2));
    assert_eq!(coefficient_bytes(0, 0, (1, 1)), 0);
    assert_eq!(
        coefficient_bytes(60_000, 1_000, (1, 1)),
        2 * 128 * 3 * 7_500 * 125
    );
}

/// An optimised encode reports its passes after the rows: the fraction never decreases, the pass
/// under way never goes back, the rows take the first pass's share and the encode ends at 1 with
/// every pass done. A baseline encode's single pass is its rows.
#[test]
fn an_optimised_encode_reports_its_passes_to_completion() {
    let (width, height) = (200, 120);
    let rgba = scene(width, height);
    for chroma in [(1, 1), (2, 2)] {
        let (_, reports) = encoded(
            width,
            height,
            &rgba,
            &settings(80, chroma, true, &[], None, None),
        );
        // Nine scans, each an optimisation pass and an output pass, after the trellis passes:
        // two for each of the three components.
        let passes = reports[0].passes;
        assert_eq!(passes, 9 * 2 + 2 * 3, "{passes} passes");
        assert!(reports.iter().all(|report| report.passes == passes));
        for pair in reports.windows(2) {
            assert!(pair[1].fraction >= pair[0].fraction, "{pair:?}");
            assert!(pair[1].pass >= pair[0].pass, "{pair:?}");
        }
        let rows: Vec<_> = reports
            .iter()
            .take_while(|report| report.pass == 0)
            .collect();
        assert_eq!(rows.last().unwrap().rows, height as usize);
        assert_eq!(rows.last().unwrap().fraction, 1.0 / f64::from(passes));
        let after: Vec<u32> = reports
            .iter()
            .map(|report| report.pass)
            .filter(|&pass| pass > 0)
            .collect();
        assert!(after.len() > 2, "the passes are reported: {after:?}");
        let last = reports.last().unwrap();
        assert_eq!((last.pass, last.fraction), (passes, 1.0));

        let (_, baseline) = encoded(
            width,
            height,
            &rgba,
            &settings(80, chroma, false, &[], None, None),
        );
        assert!(
            baseline
                .iter()
                .all(|report| report.passes == 1 && report.pass == 0)
        );
        assert_eq!(baseline.last().unwrap().fraction, 1.0);
    }
}

/// A step that stops an optimised encode between its passes stops it there: the stop comes back
/// as it is, the writer holds no finished file, and the next encode is unaffected.
#[test]
fn a_stop_between_passes_ends_an_optimised_encode() {
    let (width, height) = (200, 120);
    let rgba = scene(width, height);
    let settings = settings(80, (1, 1), true, &[], None, None);
    let (whole, _) = encoded(width, height, &rgba, &settings);
    for stop_at in [1, 3] {
        let mut out = Vec::new();
        let mut seen = Vec::new();
        let error = encode(&mut out, width, height, &rgba, &settings, &mut |progress| {
            seen.push(progress.pass);
            if progress.pass >= stop_at {
                Err(Stepped::Stop)
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(matches!(error, Stepped::Stop), "{error:?}");
        assert_eq!(
            seen.last(),
            Some(&stop_at),
            "stopped at the first report of pass {stop_at}"
        );
        assert!(
            out.len() < whole.len() && !out.ends_with(&[0xff, 0xd9]),
            "no finished file"
        );
    }
    let (again, _) = encoded(width, height, &rgba, &settings);
    assert!(again == whole, "the next encode writes the same file");
}

/// A writer that fails while an optimised encode writes its scans is a write error with the
/// writer's own reason, not an abort.
#[test]
fn a_write_failure_during_the_passes_is_the_writers_error() {
    struct Full;
    impl std::io::Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "the disk is full",
            ))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let (width, height) = (300, 200);
    let rgba = scene(width, height);
    let error = encode(
        Full,
        width,
        height,
        &rgba,
        &settings(80, (1, 1), true, &[], None, None),
        &mut |_| Ok::<_, JpegError>(()),
    )
    .unwrap_err();
    match error {
        JpegError::Write(error) => assert_eq!(error.to_string(), "the disk is full"),
        other => panic!("{other:?}"),
    }
}

/// A `width` × `height` photo-like frame: smooth sky and ground gradients, soft shading, fine grain
/// and a few hard-edged shapes.
fn photo_like(width: u32, height: u32) -> Vec<u8> {
    let mut seeded = Seeded(0x9e37_79b9 ^ u64::from(width) << 16 ^ u64::from(height));
    let shapes: Vec<(u32, u32, u32, [u8; 3])> = (0..12)
        .map(|_| {
            (
                seeded.below(width),
                seeded.below(height),
                seeded.below(width / 6 + 1) + 4,
                [
                    seeded.below(256) as u8,
                    seeded.below(256) as u8,
                    seeded.below(256) as u8,
                ],
            )
        })
        .collect();
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let (fx, fy) = (x as f32 / width as f32, y as f32 / height as f32);
            let grain = (seeded.next() % 9) as f32 - 4.0;
            let mut pixel = if fy < 0.55 {
                [90.0 + 80.0 * fy, 140.0 + 60.0 * fy, 220.0 - 40.0 * fy]
            } else {
                let shade = 20.0 * (fx * 23.0).sin() * (fy * 17.0).cos();
                [110.0 + shade, 95.0 + shade, 60.0 + shade * 0.5]
            };
            for value in &mut pixel {
                *value += grain;
            }
            for &(cx, cy, r, colour) in &shapes {
                let (dx, dy) = (x.abs_diff(cx), y.abs_diff(cy));
                if dx * dx + dy * dy < r * r {
                    pixel = colour.map(|value| f32::from(value) + grain);
                }
            }
            rgba.extend(pixel.map(|value| value.clamp(0.0, 255.0) as u8));
            rgba.push(255);
        }
    }
    rgba
}

/// Encode `rgba` baseline and optimised at each quality, answering per quality the two sizes and
/// encode times.
fn sizes(
    width: u32,
    height: u32,
    rgba: &[u8],
    chroma: (u8, u8),
) -> Vec<(u8, [usize; 2], [f64; 2])> {
    [60, 75, 85, 90, 95]
        .into_iter()
        .map(|quality| {
            let mut bytes = [0; 2];
            let mut seconds = [0.0; 2];
            for (index, optimise) in [false, true].into_iter().enumerate() {
                let started = Instant::now();
                let (file, _) = encoded(
                    width,
                    height,
                    rgba,
                    &settings(quality, chroma, optimise, &[], None, None),
                );
                seconds[index] = started.elapsed().as_secs_f64();
                bytes[index] = file.len();
            }
            (quality, bytes, seconds)
        })
        .collect()
}

/// At the same quality an optimised file is smaller than the baseline one, on photo-like frames and
/// on the supplied photographs re-encoded, at 4:4:4 and 4:2:0.
#[test]
#[slow]
fn an_optimised_file_is_smaller_at_the_same_quality() {
    let mut frames = vec![(
        "photo-like 640x427".to_owned(),
        640,
        427,
        photo_like(640, 427),
    )];
    for name in ["portrait.jpg", "srgb.jpg"] {
        let (width, height, rgba) = crate::tests::decode(&fixture(name)).unwrap();
        frames.push((name.to_owned(), width, height, rgba));
    }
    for (name, width, height, rgba) in &frames {
        for chroma in [(1, 1), (2, 2)] {
            for (quality, [baseline, optimised], [slow, fast]) in
                sizes(*width, *height, rgba, chroma)
            {
                println!(
                    "{name} {chroma:?} q{quality}: baseline {baseline} B in {:.1} ms, optimised {optimised} B in {:.1} ms ({:.1}% smaller)",
                    slow * 1000.0,
                    fast * 1000.0,
                    100.0 * (1.0 - optimised as f64 / baseline as f64)
                );
                assert!(optimised < baseline, "{name} {chroma:?} q{quality}");
            }
        }
    }
}

/// Sizes and encode times, baseline and optimised, of the supplied photographs and of any
/// photographs named in `LUXFORGE_JPEG_COMPARE` (colon-separated paths), decoded and re-encoded.
/// Run in release:
///
/// ```text
/// LUXFORGE_JPEG_COMPARE=/path/a.jpg:/path/b.jpg \
///   cargo test --release -p luxforge-jpeg --lib encode_tests::measure -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a measurement, not a gate"]
fn measure_optimised_sizes_and_times() {
    let mut paths = Vec::new();
    if let Ok(extra) = std::env::var("LUXFORGE_JPEG_COMPARE") {
        paths.extend(extra.split(':').map(std::path::PathBuf::from));
    }
    for path in paths {
        let bytes = std::fs::read(&path).unwrap();
        let (width, height, rgba) = crate::tests::decode(&bytes).unwrap();
        for chroma in [(1, 1), (2, 2)] {
            for (quality, [baseline, optimised], [slow, fast]) in
                sizes(width, height, &rgba, chroma)
            {
                println!(
                    "{} {width}x{height} {chroma:?} q{quality}: baseline {baseline} B {:.0} ms, optimised {optimised} B {:.0} ms, {:.1}% smaller",
                    path.display(),
                    slow * 1000.0,
                    fast * 1000.0,
                    100.0 * (1.0 - optimised as f64 / baseline as f64)
                );
            }
        }
    }
}
