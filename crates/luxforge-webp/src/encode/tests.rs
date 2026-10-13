use super::*;
use image_webp::WebPDecoder;
use std::io::Cursor;

/// Gradients and a hard edge, at a size whose sides are odd and not multiples of 16, so the
/// encoder's edge macroblocks and the odd chroma column are exercised.
fn frame(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let r = (x * 255 / width.max(1)) as u8;
            let g = (y * 255 / height.max(1)) as u8;
            let b = if x * 2 < width { 40 } else { 210 };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    rgba
}

fn settings(quality: u8, optimise: bool) -> Settings<'static> {
    Settings {
        quality,
        optimise,
        icc: None,
        exif: None,
    }
}

fn encode_quietly(width: u32, height: u32, rgba: &[u8], settings: &Settings<'_>) -> Vec<u8> {
    encode(width, height, rgba, settings, &mut |_| true).expect("encodes")
}

/// The RIFF chunks of a WebP file, as `(fourcc, payload)`.
fn chunks(file: &[u8]) -> Vec<([u8; 4], &[u8])> {
    assert_eq!(&file[0..4], b"RIFF");
    let size = u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize;
    assert_eq!(size + 8, file.len(), "the RIFF size covers the file");
    assert_eq!(&file[8..12], b"WEBP");
    let mut found = Vec::new();
    let mut at = 12;
    while at < file.len() {
        let fourcc: [u8; 4] = file[at..at + 4].try_into().unwrap();
        let len = u32::from_le_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
        found.push((fourcc, &file[at + 8..at + 8 + len]));
        at += 8 + len + len % 2;
    }
    found
}

/// Per-channel mean and maximum absolute error of decoded RGB against the frame.
fn error(rgba: &[u8], rgb: &[u8]) -> (f64, u8) {
    let mut sum = 0u64;
    let mut max = 0u8;
    for (original, decoded) in rgba.chunks_exact(4).zip(rgb.chunks_exact(3)) {
        for channel in 0..3 {
            let diff = original[channel].abs_diff(decoded[channel]);
            sum += u64::from(diff);
            max = max.max(diff);
        }
    }
    (sum as f64 / (rgb.len() as f64), max)
}

#[test]
fn a_lossy_opaque_file_decodes_to_the_frame_within_tolerance() {
    for optimise in [false, true] {
        let (width, height) = (257, 131);
        let rgba = frame(width, height);
        let file = encode_quietly(width, height, &rgba, &settings(90, optimise));
        let mut decoder = WebPDecoder::new(Cursor::new(&file)).expect("a WebP file");
        assert_eq!(decoder.dimensions(), (width, height));
        assert!(decoder.is_lossy(), "lossy (VP8)");
        assert!(!decoder.has_alpha(), "no alpha plane for an opaque frame");
        let mut rgb = vec![0; decoder.output_buffer_size().unwrap()];
        decoder.read_image(&mut rgb).expect("decodes");
        // Measured at quality 90 on this pattern: a mean of about 1.2 and a maximum of 64, at the
        // hard blue edge, where 4:2:0 halves the chroma; the bounds leave room for libwebp updates.
        let (mean, max) = error(&rgba, &rgb);
        assert!(mean < 3.0, "optimise {optimise}: mean error {mean}");
        assert!(max <= 96, "optimise {optimise}: max error {max}");
    }
}

#[test]
fn the_profile_and_exif_are_the_iccp_and_exif_chunks_of_a_vp8x_container() {
    let rgba = frame(64, 48);
    let icc = b"an ICC profile of odd length".as_slice();
    let exif = b"II*\0\x08\0\0\0\0\0\0\0\0\0".as_slice();
    let file = encode_quietly(
        64,
        48,
        &rgba,
        &Settings {
            icc: Some(icc),
            exif: Some(exif),
            ..settings(80, false)
        },
    );
    let found = chunks(&file);
    let fourccs: Vec<&[u8; 4]> = found.iter().map(|(fourcc, _)| fourcc).collect();
    assert_eq!(fourccs, [b"VP8X", b"ICCP", b"VP8 ", b"EXIF"]);
    let vp8x = found[0].1;
    // Flags: ICC (bit 5) and EXIF (bit 3), nothing else; then the canvas less one, 24 bits each.
    assert_eq!(vp8x[0], 0b0010_1000);
    assert_eq!(&vp8x[4..10], &[63, 0, 0, 47, 0, 0]);
    assert_eq!(found[1].1, icc);
    assert_eq!(found[3].1, exif);
    let mut decoder = WebPDecoder::new(Cursor::new(&file)).unwrap();
    assert_eq!(decoder.icc_profile().unwrap().as_deref(), Some(icc));
    assert_eq!(decoder.exif_metadata().unwrap().as_deref(), Some(exif));
}

#[test]
fn without_chunks_the_file_is_the_simple_container() {
    let rgba = frame(16, 16);
    let file = encode_quietly(16, 16, &rgba, &settings(75, false));
    let found = chunks(&file);
    assert_eq!(found.len(), 1);
    assert_eq!(&found[0].0, b"VP8 ");
}

/// The optimised encode's sharp conversion of the borrowed frame writes the bytes libwebp's own
/// sharp path writes from its ARGB copy of the frame.
#[test]
fn the_sharp_conversion_of_the_borrowed_frame_is_libwebps_own() {
    for (width, height) in [(257, 131), (4, 4), (17, 5)] {
        let rgba = frame(width, height);
        let settings = settings(82, true);
        let direct = vp8(
            width,
            height,
            &rgba,
            &settings,
            Conversion::Sharp,
            &mut |_| true,
        );
        let reference = vp8(
            width,
            height,
            &rgba,
            &settings,
            Conversion::SharpThroughArgb,
            &mut |_| true,
        );
        assert!(
            direct.unwrap() == reference.unwrap(),
            "{width}x{height}: the bitstreams differ"
        );
    }
    // And the sharp conversion is not the plain one.
    let rgba = frame(257, 131);
    let sharp = vp8(
        257,
        131,
        &rgba,
        &settings(82, true),
        Conversion::Sharp,
        &mut |_| true,
    );
    let plain = vp8(
        257,
        131,
        &rgba,
        &settings(82, true),
        Conversion::Plain,
        &mut |_| true,
    );
    assert!(sharp.unwrap() != plain.unwrap());
}

#[test]
fn small_frames_encode_with_and_without_optimise() {
    for (width, height) in [(1, 1), (3, 3), (2, 9)] {
        let rgba = frame(width, height);
        for optimise in [false, true] {
            let file = encode_quietly(width, height, &rgba, &settings(50, optimise));
            let decoder = WebPDecoder::new(Cursor::new(&file)).unwrap();
            assert_eq!(decoder.dimensions(), (width, height));
        }
    }
}

#[test]
fn progress_rises_to_100_and_false_stops_the_encode() {
    let rgba = frame(257, 131);
    let mut seen = Vec::new();
    encode(257, 131, &rgba, &settings(80, true), &mut |percent| {
        seen.push(percent);
        true
    })
    .unwrap();
    assert!(seen.windows(2).all(|pair| pair[0] <= pair[1]), "{seen:?}");
    assert_eq!(seen.last(), Some(&100), "{seen:?}");

    let mut calls = 0;
    let stopped = encode(257, 131, &rgba, &settings(80, true), &mut |_| {
        calls += 1;
        calls < 3
    });
    assert_eq!(stopped, Err(Error::Stopped));
    assert_eq!(calls, 3, "no report after the one that stopped");
}

#[test]
fn cancellation_at_the_final_progress_report_stops_the_encode() {
    let rgba = frame(17, 13);
    for optimise in [false, true] {
        let mut seen = Vec::new();
        let result = encode(17, 13, &rgba, &settings(80, optimise), &mut |percent| {
            seen.push(percent);
            percent < 100
        });
        assert_eq!(seen.last(), Some(&100));
        assert_eq!(result, Err(Error::Stopped), "optimise {optimise}");
    }
}

#[test]
fn a_panic_in_progress_is_resumed_after_libwebp_returns() {
    let rgba = frame(64, 64);
    let caught = std::panic::catch_unwind(|| {
        let _ = encode(64, 64, &rgba, &settings(80, false), &mut |_| {
            panic!("the progress callback panicked")
        });
    });
    let panic = caught.unwrap_err();
    assert_eq!(
        panic.downcast_ref::<&str>(),
        Some(&"the progress callback panicked")
    );
    // libwebp is still usable.
    encode_quietly(64, 64, &rgba, &settings(80, false));
}

#[test]
fn frames_and_settings_libwebp_cannot_take_are_refused_before_it_is_called() {
    let rgba = frame(8, 8);
    let refused = |width, height, rgba: &[u8], quality| {
        let mut called = false;
        let result = encode(width, height, rgba, &settings(quality, false), &mut |_| {
            called = true;
            true
        });
        assert!(!called);
        result.unwrap_err()
    };
    assert_eq!(refused(0, 8, &[], 80), Error::Invalid("the frame is empty"));
    assert_eq!(
        refused(MAX_SIDE + 1, 1, &vec![0; (MAX_SIDE as usize + 1) * 4], 80),
        Error::Invalid("a side is over 16,383 pixels")
    );
    assert_eq!(
        refused(1, MAX_SIDE + 1, &vec![0; (MAX_SIDE as usize + 1) * 4], 80),
        Error::Invalid("a side is over 16,383 pixels")
    );
    assert_eq!(
        refused(8, 8, &rgba[4..], 80),
        Error::Invalid("the frame is not width × height RGBA8 pixels")
    );
    assert_eq!(
        refused(8, 8, &rgba, 101),
        Error::Invalid("the quality is over 100")
    );
}

#[test]
fn the_longest_side_encodes() {
    let rgba = frame(MAX_SIDE, 2);
    let file = encode_quietly(MAX_SIDE, 2, &rgba, &settings(30, false));
    let decoder = WebPDecoder::new(Cursor::new(&file)).unwrap();
    assert_eq!(decoder.dimensions(), (MAX_SIDE, 2));
}
