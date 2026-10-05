use crate::{
    icc::{ICC_HEADER, MAX_CHUNK_DATA, reassemble},
    warnings::{self, Verdict, WARNINGS},
    *,
};
use image::{ImageDecoder, codecs::jpeg::JpegDecoder};
use mozjpeg::{ColorSpace, Compress};
use mozjpeg_sys::{
    JWRN_ADOBE_XFORM, JWRN_BOGUS_ICC, JWRN_BOGUS_PROGRESSION, JWRN_EXTRANEOUS_DATA,
    JWRN_HIT_MARKER, JWRN_HUFF_BAD_CODE, JWRN_JFIF_MAJOR, JWRN_JPEG_EOF, JWRN_MUST_RESYNC,
    JWRN_NOT_SEQUENTIAL, JWRN_TOO_MUCH_DATA,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    io::{Cursor, Write},
    path::Path,
    rc::Rc,
};

const LIMITS: Limits = Limits {
    max_side: 16384,
    max_pixels: 64_000_000,
};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/s0")
            .join(name),
    )
    .unwrap()
}

/// The whole image decoded through the crate, unoriented, as RGBA.
fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), JpegError> {
    let mut decoder = Decoder::new(bytes, LIMITS)?;
    let (width, height) = (decoder.width(), decoder.height());
    let mut rgba = vec![0; width as usize * height as usize * 4];
    decoder.read_rows(&mut rgba)?;
    decoder.finish()?;
    Ok((width, height, rgba))
}

/// The warnings this thread's decodes went on past since the last call.
fn accepted() -> Vec<i32> {
    decode::ACCEPTED.with(|accepted| std::mem::take(&mut *accepted.borrow_mut()))
}

/// The warning code a decode was refused for.
fn refused_for(bytes: &[u8]) -> i32 {
    match decode(bytes) {
        Err(JpegError::Corrupt(code)) => code,
        other => panic!("expected a refused warning, got {:?}", other.map(|_| ())),
    }
}

/// An independent decode of the same bytes (`image`'s JPEG decoder), unoriented, as RGB.
fn independent(bytes: &[u8]) -> (u32, u32, Vec<u8>) {
    let decoder = JpegDecoder::new(Cursor::new(bytes)).unwrap();
    let (width, height) = decoder.dimensions();
    let grey = decoder.color_type() == image::ColorType::L8;
    let mut pixels = vec![0; decoder.total_bytes() as usize];
    decoder.read_image(&mut pixels).unwrap();
    let rgb = if grey {
        pixels.iter().flat_map(|&value| [value; 3]).collect()
    } else {
        pixels
    };
    (width, height, rgb)
}

/// A `width` × `height` pattern encoded by libjpeg at quality 85 and 4:2:0, baseline or with its
/// simple progression script. The file starts with SOI and libjpeg's 16-byte JFIF APP0.
fn pattern(width: usize, height: usize, progressive: bool) -> Vec<u8> {
    let rgb: Vec<u8> = (0..width * height)
        .flat_map(|i| {
            let (x, y) = (i % width, i / width);
            [(x * 4) as u8, (y * 7) as u8, ((x ^ y) * 3) as u8]
        })
        .collect();
    let mut compress = Compress::new(ColorSpace::JCS_RGB);
    compress.set_fastest_defaults();
    compress.set_size(width, height);
    compress.set_quality(85.0);
    if progressive {
        compress.set_progressive_mode();
    }
    let mut started = compress.start_compress(Vec::new()).unwrap();
    started.write_scanlines(&rgb).unwrap();
    started.finish().unwrap()
}

/// Where the JFIF APP0 `pattern` writes ends: SOI, then `FF E0`, its length 16 and 14 bytes.
const AFTER_APP0: usize = 2 + 2 + 16;

/// The offset of the first SOS marker, and of its scan's entropy-coded data.
fn first_scan(bytes: &[u8]) -> (usize, usize) {
    let mut i = 2;
    loop {
        let marker = bytes[i + 1];
        let size = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        if marker == 0xda {
            return (i, i + 2 + size);
        }
        i += 2 + size;
    }
}

/// The offset of the first marker at or after `from` in entropy-coded data: `FF` followed by
/// neither a stuffed `00` nor a restart marker.
fn next_marker(bytes: &[u8], from: usize) -> usize {
    (from..bytes.len() - 1)
        .find(|&i| bytes[i] == 0xff && !matches!(bytes[i + 1], 0x00 | 0xd0..=0xd7))
        .unwrap()
}

fn spliced(bytes: &[u8], at: usize, remove: usize, insert: &[u8]) -> Vec<u8> {
    [&bytes[..at], insert, &bytes[at + remove..]].concat()
}

#[test]
fn every_supported_fixture_decodes_to_its_dimensions_and_opaque_rgba() {
    for (name, size, grey) in [
        ("orientation-1.jpg", (480, 320), false),
        ("orientation-6.jpg", (480, 320), false),
        ("portrait.jpg", (320, 480), false),
        ("srgb.jpg", (480, 320), false),
        ("greyscale.jpg", (480, 320), true),
        ("jpeg-scan-420-noninterleaved.jpg", (48, 32), false),
    ] {
        let (width, height, rgba) = decode(&fixture(name)).unwrap();
        assert_eq!((width, height), size, "{name}");
        assert!(rgba.chunks_exact(4).all(|pixel| pixel[3] == 255), "{name}");
        if grey {
            assert!(
                rgba.chunks_exact(4)
                    .all(|pixel| pixel[0] == pixel[1] && pixel[1] == pixel[2]),
                "{name}: greyscale expands to grey RGB"
            );
        }
    }
    assert!(accepted().is_empty(), "no committed fixture warns");
}

/// A progressive file (SOF2) decodes to exactly the bytes of the baseline file holding the same
/// quantized coefficients, at 4:2:0: libjpeg's simple progression script refines every
/// coefficient to full precision, and both decode through the same IDCT and upsampling.
#[test]
fn a_progressive_file_decodes_as_its_baseline_twin() {
    let (baseline, progressive) = (pattern(61, 37, false), pattern(61, 37, true));
    assert!(progressive.windows(2).any(|pair| pair == [0xff, 0xc2]));
    let (_, _, expected) = decode(&baseline).unwrap();
    let (_, _, actual) = decode(&progressive).unwrap();
    assert!(actual == expected);
}

/// Restart markers decode to the bytes of the same coefficients without them: the committed
/// file is `jpeg-scan-420-interleaved.jpg` rewritten losslessly with one restart interval per MCU.
#[test]
fn restart_intervals_decode_as_the_same_file_without_them() {
    let restarts = fixture("jpeg-restart.jpg");
    assert!(restarts.windows(2).any(|pair| pair == [0xff, 0xdd]), "DRI");
    let (_, _, expected) = decode(&fixture("jpeg-scan-420-interleaved.jpg")).unwrap();
    let (_, _, actual) = decode(&restarts).unwrap();
    assert!(actual == expected);
}

#[test]
fn colour_spaces_and_limits_are_checked_before_pixels() {
    let cmyk = Decoder::new(&fixture("cmyk.jpg"), LIMITS).err().unwrap();
    assert!(matches!(cmyk, JpegError::ColourSpace), "{cmyk:?}");
    let small = Limits {
        max_side: 479,
        max_pixels: 64_000_000,
    };
    let wide = Decoder::new(&fixture("orientation-1.jpg"), small)
        .err()
        .unwrap();
    assert!(matches!(wide, JpegError::Dimensions), "{wide:?}");
    let few = Limits {
        max_side: 16384,
        max_pixels: 480 * 320 - 1,
    };
    let many = Decoder::new(&fixture("orientation-1.jpg"), few)
        .err()
        .unwrap();
    assert!(matches!(many, JpegError::Dimensions), "{many:?}");
    let oversized = Decoder::new(&fixture("oversized.jpg"), LIMITS)
        .err()
        .unwrap();
    assert!(matches!(oversized, JpegError::Dimensions), "{oversized:?}");
}

/// Every listed code is one of this build's warnings, each named as libjpeg names it, and every
/// warning `jerror.h` declares is listed; a code the table does not list refuses.
#[test]
fn the_warning_table_covers_every_warning_and_fails_closed() {
    let every = [
        (JWRN_ADOBE_XFORM, "JWRN_ADOBE_XFORM"),
        (JWRN_BOGUS_PROGRESSION, "JWRN_BOGUS_PROGRESSION"),
        (JWRN_EXTRANEOUS_DATA, "JWRN_EXTRANEOUS_DATA"),
        (JWRN_HIT_MARKER, "JWRN_HIT_MARKER"),
        (JWRN_HUFF_BAD_CODE, "JWRN_HUFF_BAD_CODE"),
        (JWRN_JFIF_MAJOR, "JWRN_JFIF_MAJOR"),
        (JWRN_JPEG_EOF, "JWRN_JPEG_EOF"),
        (JWRN_MUST_RESYNC, "JWRN_MUST_RESYNC"),
        (JWRN_NOT_SEQUENTIAL, "JWRN_NOT_SEQUENTIAL"),
        (JWRN_TOO_MUCH_DATA, "JWRN_TOO_MUCH_DATA"),
        (JWRN_BOGUS_ICC, "JWRN_BOGUS_ICC"),
    ];
    assert_eq!(WARNINGS.len(), every.len());
    for (code, name) in every {
        assert_eq!(warnings::name(code), Some(name));
    }
    let accepted: Vec<_> = WARNINGS
        .iter()
        .filter(|warning| warning.verdict != Verdict::Refuse)
        .map(|warning| (warning.name, warning.verdict))
        .collect();
    assert_eq!(
        accepted,
        [
            ("JWRN_EXTRANEOUS_DATA", Verdict::AcceptInHeader),
            ("JWRN_JFIF_MAJOR", Verdict::Accept),
            ("JWRN_BOGUS_ICC", Verdict::Accept),
        ]
    );
    assert!(warnings::accepts(JWRN_EXTRANEOUS_DATA, true));
    assert!(!warnings::accepts(JWRN_EXTRANEOUS_DATA, false));
    for unknown in [0, -1, 7, 999] {
        assert!(!warnings::accepts(unknown, true), "{unknown}");
    }
    assert_eq!(
        JpegError::Corrupt(999).to_string(),
        "corrupt or truncated JPEG data (libjpeg warning 999)"
    );
}

/// Harmless irregularities decode to exactly the pixels of the clean file, and the warning each
/// raises is seen and let through: bytes that are not a marker between two header segments, a
/// stuffed `FF 00` among them, and a JFIF marker of major version 2.
#[test]
fn a_harmless_warning_decodes_the_same_pixels() {
    let clean = pattern(61, 37, false);
    let (_, _, expected) = decode(&clean).unwrap();
    assert!(accepted().is_empty());

    let extraneous = spliced(&clean, AFTER_APP0, 0, &[0x12, 0x34, 0xff, 0x00, 0x56]);
    let (_, _, actual) = decode(&extraneous).unwrap();
    assert!(actual == expected, "extra bytes between header segments");
    assert_eq!(accepted(), [JWRN_EXTRANEOUS_DATA]);

    let mut jfif_2 = clean.clone();
    assert_eq!(&jfif_2[6..11], b"JFIF\0");
    jfif_2[11] = 2;
    let (_, _, actual) = decode(&jfif_2).unwrap();
    assert!(actual == expected, "JFIF 2.x");
    assert_eq!(accepted(), [JWRN_JFIF_MAJOR]);

    // The progressive decode reads every scan before the first row; bytes between header
    // segments are let through there too.
    let progressive = pattern(61, 37, true);
    let extraneous = spliced(&progressive, AFTER_APP0, 0, &[0; 3]);
    let (_, _, actual) = decode(&extraneous).unwrap();
    assert!(actual == expected, "progressive, extra bytes in the header");
    assert_eq!(accepted(), [JWRN_EXTRANEOUS_DATA]);
}

/// Each warning that stands for missing, corrupt or guessed data refuses the decode with its own
/// code, and a decode after a refused one works, so nothing a failure left behind is reused.
#[test]
fn a_data_loss_warning_refuses_the_decode() {
    let clean = pattern(61, 37, false);
    let (scan_marker, scan) = first_scan(&clean);
    let end = clean.len() - 2;
    let middle = scan + (end - scan) / 2;

    // The data stops mid-scan at an EOI.
    let cut = spliced(&clean, middle, end - middle, &[]);
    assert_eq!(refused_for(&cut), JWRN_HIT_MARKER, "EOI mid-scan");
    // A restart marker in a stream without restarts cuts the scan short too.
    let rst = spliced(&clean, middle, 0, &[0xff, 0xd0]);
    assert_eq!(refused_for(&rst), JWRN_HIT_MARKER, "stray restart marker");
    // All ones (stuffed FF 00 bytes) is no Huffman code.
    let ones = [0xff, 0x00].repeat(8);
    let bad = spliced(&clean, middle, ones.len(), &ones);
    assert_eq!(refused_for(&bad), JWRN_HUFF_BAD_CODE, "bad Huffman code");
    // Bytes left over after the last scan are what a decode out of step leaves.
    let trailing = spliced(&clean, end, 0, &[0x55; 32]);
    assert_eq!(
        refused_for(&trailing),
        JWRN_EXTRANEOUS_DATA,
        "extra bytes after the scan"
    );
    // A DRI segment after the scan that takes the EOI for its value: libjpeg meets the end of the
    // data looking for the next marker.
    let overrun = spliced(&clean, end, 0, &[0xff, 0xdd, 0, 4]);
    assert_eq!(
        refused_for(&overrun),
        JWRN_JPEG_EOF,
        "premature end of file"
    );
    // Ss, Se and Ah/Al, the SOS payload's last three bytes, zeroed in a baseline scan.
    let mut partial = clean.clone();
    partial[scan - 3..scan].fill(0);
    assert_eq!(scan - scan_marker, 2 + 2 + 10, "a three-component SOS");
    assert_eq!(refused_for(&partial), JWRN_NOT_SEQUENTIAL, "partial band");
    // An Adobe marker with an unknown transform in place of the JFIF marker.
    let adobe = [
        [0xff, 0xee, 0, 14].as_slice(),
        b"Adobe",
        &[0, 100, 0, 0, 0, 0, 5],
    ]
    .concat();
    let guessed = spliced(&clean, 2, AFTER_APP0 - 2, &adobe);
    assert_eq!(refused_for(&guessed), JWRN_ADOBE_XFORM, "colour transform");

    // A restart marker out of order.
    let restarts = fixture("jpeg-restart.jpg");
    let (_, restart_scan) = first_scan(&restarts);
    let first_restart = (restart_scan..restarts.len())
        .find(|&i| restarts[i] == 0xff && restarts[i + 1] == 0xd0)
        .unwrap();
    let mut resync = restarts.clone();
    resync[first_restart + 1] = 0xd5;
    assert_eq!(
        refused_for(&resync),
        JWRN_MUST_RESYNC,
        "restart out of order"
    );

    // A progressive file whose first scan (every component's DC) is gone: the AC scans that
    // follow refine coefficients no scan began.
    let progressive = pattern(61, 37, true);
    let (dc, dc_data) = first_scan(&progressive);
    let no_dc = spliced(
        &progressive,
        dc,
        next_marker(&progressive, dc_data) - dc,
        &[],
    );
    assert_eq!(
        refused_for(&no_dc),
        JWRN_BOGUS_PROGRESSION,
        "a scan missing"
    );

    assert!(accepted().is_empty(), "nothing was let through");
    assert!(decode(&clean).is_ok());
}

/// A file cut short with no EOI is refused by the container walk before libjpeg sees it, and a
/// libjpeg fatal error (here a scan naming a component the frame does not have) is an error and
/// never an abort.
#[test]
fn truncation_and_a_libjpeg_fatal_error_are_errors_not_aborts() {
    let valid = fixture("orientation-1.jpg");
    let (_, scan) = first_scan(&valid);
    let cut = &valid[..scan + (valid.len() - scan) / 2];
    assert!(matches!(decode(cut), Err(JpegError::Malformed(_))));
    let truncated = decode(&fixture("truncated.jpg")).err().unwrap();
    assert!(
        matches!(
            truncated,
            JpegError::Malformed(_) | JpegError::Corrupt(JWRN_HIT_MARKER)
        ),
        "{truncated:?}"
    );

    let (sos, _) = first_scan(&valid);
    let mut broken = valid.clone();
    // The SOS payload's first bytes are the component count and the first component's selector.
    broken[sos + 5] = 0x7f;
    let error = decode(&broken).err().unwrap();
    assert!(matches!(error, JpegError::Undecodable), "{error:?}");
    assert_eq!(error.to_string(), "JPEG data libjpeg cannot decode");
    assert!(matches!(
        Decoder::new(b"not a jpeg", LIMITS),
        Err(JpegError::Malformed(_))
    ));
    assert!(decode(&valid).is_ok());
}

#[test]
fn rows_are_read_in_any_whole_strips_and_never_past_the_end() {
    let bytes = fixture("orientation-1.jpg");
    let (_, _, whole) = decode(&bytes).unwrap();
    let mut decoder = Decoder::new(&bytes, LIMITS).unwrap();
    let stride = 480 * 4;
    let mut strips = Vec::new();
    for rows in [1, 16, 7, 296] {
        let mut strip = vec![0; rows * stride];
        decoder.read_rows(&mut strip).unwrap();
        strips.extend(strip);
    }
    assert_eq!(strips, whole);
    // Rows past the end are refused before libjpeg would answer them with JWRN_TOO_MUCH_DATA.
    let past = decoder.read_rows(&mut vec![0; stride]).unwrap_err();
    assert!(matches!(past, JpegError::Internal(_)));
    assert!(matches!(
        decoder.read_rows(&mut [0; 3]).unwrap_err(),
        JpegError::Internal(_)
    ));
    decoder.finish().unwrap();

    let early = Decoder::new(&bytes, LIMITS).unwrap();
    assert!(matches!(
        early.finish().unwrap_err(),
        JpegError::Internal(_)
    ));
}

fn icc_segment(sequence: u8, count: u8, data: &[u8]) -> Vec<u8> {
    [ICC_HEADER, &[sequence, count], data].concat()
}

#[test]
fn icc_chunks_reassemble_in_sequence_order_and_a_broken_sequence_is_refused() {
    let first = icc_segment(1, 3, b"abc");
    let second = icc_segment(2, 3, b"def");
    let third = icc_segment(3, 3, b"g");
    let other = b"MPF\0something".to_vec();
    let profile = reassemble(
        [&third, &other, &first, &second]
            .into_iter()
            .map(Vec::as_slice),
    )
    .unwrap();
    assert_eq!(profile.as_deref(), Some(b"abcdefg".as_slice()));
    assert_eq!(
        reassemble([other.as_slice()].into_iter()).unwrap(),
        None,
        "no ICC chunk, no profile"
    );
    for (what, segments) in [
        ("numbered from 0", vec![icc_segment(0, 1, b"x")]),
        ("number above the count", vec![icc_segment(2, 1, b"x")]),
        (
            "repeated number",
            vec![icc_segment(1, 2, b"x"), icc_segment(1, 2, b"y")],
        ),
        ("missing chunk", vec![icc_segment(1, 2, b"x")]),
        (
            "counts differ",
            vec![icc_segment(1, 2, b"x"), icc_segment(2, 3, b"y")],
        ),
        ("no chunk header", vec![ICC_HEADER.to_vec()]),
    ] {
        let error = reassemble(segments.iter().map(Vec::as_slice)).unwrap_err();
        assert!(matches!(error, JpegError::Icc(_)), "{what}");
    }
    let big = vec![0; MAX_CHUNK_DATA];
    let chunks: Vec<_> = (1..=17).map(|n| icc_segment(n, 17, &big)).collect();
    let error = reassemble(chunks.iter().map(Vec::as_slice)).unwrap_err();
    assert!(matches!(error, JpegError::Icc(_)));
}

/// The committed ICC fixture's profile comes back byte for byte as the independent decoder reads
/// it, and a file without one has none.
#[test]
fn the_icc_profile_matches_an_independent_reader() {
    let bytes = fixture("srgb.jpg");
    let decoder = Decoder::new(&bytes, LIMITS).unwrap();
    let mut independent = JpegDecoder::new(Cursor::new(&bytes)).unwrap();
    assert_eq!(
        decoder.icc_profile().map(<[u8]>::to_vec),
        independent.icc_profile().unwrap()
    );
    assert!(decoder.icc_profile().is_some());
    let untagged = fixture("orientation-1.jpg");
    assert_eq!(Decoder::new(&untagged, LIMITS).unwrap().icc_profile(), None);
}

/// The APP2 chunks of each segment of an encoded file, as `(sequence, count, data length)`.
fn written_icc_chunks(bytes: &[u8]) -> Vec<(u8, u8, usize)> {
    segments(bytes)
        .map(Result::unwrap)
        .filter(|segment| segment.marker == 0xe2)
        .filter_map(|segment| segment.payload.strip_prefix(ICC_HEADER))
        .map(|chunk| (chunk[0], chunk[1], chunk.len() - 2))
        .collect()
}

fn encode_with(icc: Option<&[u8]>, segments: &[(u8, &[u8])]) -> Result<Vec<u8>, JpegError> {
    let rgba = vec![128; 16 * 16 * 4];
    let settings = Settings {
        quality: 90,
        chroma: (1, 1),
        pixels_per_inch: None,
        segments,
        icc,
    };
    let mut out = Vec::new();
    encode(&mut out, 16, 16, &rgba, &settings, &mut |_| {
        Ok::<_, JpegError>(())
    })?;
    Ok(out)
}

/// The encoder writes a profile as its chunks numbered from 1, after the caller's segments, one
/// chunk for a small profile and as many as a large one takes; both read back byte for byte
/// through this crate's decoder and the independent one.
#[test]
fn an_icc_profile_is_written_as_chunks_numbered_from_one() {
    let small: Vec<u8> = (0..3000).map(|i| (i % 251) as u8).collect();
    let large: Vec<u8> = (0..2 * MAX_CHUNK_DATA + 10)
        .map(|i| (i % 253) as u8)
        .collect();
    let exif = b"Exif\0\0MM\0*".as_slice();
    for (profile, expected) in [
        (&small, vec![(1, 1, 3000)]),
        (
            &large,
            vec![(1, 3, MAX_CHUNK_DATA), (2, 3, MAX_CHUNK_DATA), (3, 3, 10)],
        ),
    ] {
        let bytes = encode_with(Some(profile), &[(1, exif)]).unwrap();
        assert_eq!(written_icc_chunks(&bytes), expected);
        let markers: Vec<u8> = segments(&bytes)
            .map(|segment| segment.unwrap().marker)
            .filter(|marker| (0xe0..=0xef).contains(marker))
            .collect();
        assert_eq!(markers[..2], [0xe0, 0xe1], "JFIF, then the caller's EXIF");
        assert!(markers[2..].iter().all(|&marker| marker == 0xe2));
        let decoder = Decoder::new(&bytes, LIMITS).unwrap();
        assert_eq!(decoder.icc_profile(), Some(profile.as_slice()));
        let mut independent = JpegDecoder::new(Cursor::new(&bytes)).unwrap();
        assert_eq!(independent.icc_profile().unwrap().as_ref(), Some(profile));
    }
    assert!(written_icc_chunks(&encode_with(None, &[]).unwrap()).is_empty());
    for (what, profile) in [
        ("empty", Vec::new()),
        ("past 255 chunks", vec![0; 255 * MAX_CHUNK_DATA + 1]),
    ] {
        let error = encode_with(Some(&profile), &[]).unwrap_err();
        assert!(matches!(error, JpegError::Internal(_)), "{what}: {error:?}");
    }
}

/// The JFIF header names a density in pixels per inch when one is asked for, and libjpeg's
/// unitless 1:1 aspect ratio otherwise: units, then the horizontal and vertical densities.
#[test]
fn the_jfif_header_carries_the_density_asked_for() {
    let jfif = |pixels_per_inch| {
        let settings = Settings {
            quality: 90,
            chroma: (1, 1),
            pixels_per_inch,
            segments: &[],
            icc: None,
        };
        let mut out = Vec::new();
        encode(
            &mut out,
            16,
            16,
            &[128; 16 * 16 * 4],
            &settings,
            &mut |_| Ok::<_, JpegError>(()),
        )
        .unwrap();
        let app0 = segments(&out)
            .map(Result::unwrap)
            .find(|segment| segment.marker == 0xe0)
            .expect("a JFIF header");
        assert_eq!(&app0.payload[..5], b"JFIF\0");
        app0.payload[7..12].to_vec()
    };
    assert_eq!(jfif(None), [0, 0, 1, 0, 1]);
    assert_eq!(jfif(Some(144)), [1, 0, 144, 0, 144]);
}

/// libjpeg-turbo's integer IDCT, fancy upsampling and colour conversion are specified to give the
/// same bytes with or without SIMD, and on every platform; these digests were taken on the M4 (the
/// NEON build) and pin that, so a platform whose build decodes differently fails here: the
/// portable C path on x86_64, and Windows. The files cover 4:2:0 (two-dimensional fancy
/// upsampling) at two sizes, 4:2:2 (horizontal), 4:4:4 and greyscale.
#[test]
fn decoded_pixels_are_the_same_on_every_platform() {
    let digests: Vec<_> = [
        "orientation-1.jpg",
        "portrait.jpg",
        "greyscale.jpg",
        "jpeg-scan-420-interleaved.jpg",
        "jpeg-scan-422-interleaved.jpg",
        "jpeg-scan-444-interleaved.jpg",
    ]
    .into_iter()
    .map(|name| {
        let (_, _, rgba) = decode(&fixture(name)).unwrap();
        (name.to_owned(), format!("{:x}", Sha256::digest(&rgba)))
    })
    .collect();
    println!("{digests:#?}");
    let pinned: Vec<_> = PINNED_DIGESTS
        .iter()
        .map(|(name, digest)| (name.to_string(), digest.to_string()))
        .collect();
    assert_eq!(digests, pinned);
}

const PINNED_DIGESTS: [(&str, &str); 6] = [
    (
        "orientation-1.jpg",
        "017309d8922397c541f3bb79fd30da70f61d6ddfe0804e59e8a12a6aac14ee63",
    ),
    (
        "portrait.jpg",
        "5b34faf3a9a7976745e130bdca1da3d8a31798b61e209dad709a85480e6f66e3",
    ),
    (
        "greyscale.jpg",
        "8a5cc518a70221b531554e4438966d34c1689f01db9e77cc2672f6f080ee8f0c",
    ),
    (
        "jpeg-scan-420-interleaved.jpg",
        "21ff106d905222cc39f97fed0c5432f2aeaffa43386cffa1b3ac5d34860dcc92",
    ),
    (
        "jpeg-scan-422-interleaved.jpg",
        "32911bd013c0be3536bd738099cfe7f59c81e82223fed722acd4c46e8ebb5a37",
    ),
    (
        "jpeg-scan-444-interleaved.jpg",
        "09bee61785b80c16350ee28b6823d3f00c1fd1fcbfb7a668c6a76eba3dceb522",
    ),
];

/// What a test's step returns: its own stop, or the crate's error.
#[derive(Debug)]
enum Stepped {
    Stop,
    Jpeg(JpegError),
}

impl From<JpegError> for Stepped {
    fn from(error: JpegError) -> Self {
        Self::Jpeg(error)
    }
}

/// A step error abandons an encode and is returned unchanged, after the rows written so far; a
/// segment too large for one marker is refused before anything is written.
#[test]
fn an_encode_stops_where_its_step_says() {
    let rgba = vec![128; 40 * 40 * 4];
    let settings = Settings {
        quality: 90,
        chroma: (1, 1),
        pixels_per_inch: None,
        segments: &[],
        icc: None,
    };
    let mut seen = Vec::new();
    let mut out = Vec::new();
    let error = encode(&mut out, 40, 40, &rgba, &settings, &mut |rows| {
        seen.push(rows);
        if rows >= 32 {
            Err(Stepped::Stop)
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert!(matches!(error, Stepped::Stop));
    assert_eq!(seen, [0, 16, 32]);

    let mut seen = Vec::new();
    let mut whole = Vec::new();
    encode(&mut whole, 40, 40, &rgba, &settings, &mut |rows| {
        seen.push(rows);
        Ok::<_, Stepped>(())
    })
    .unwrap();
    assert_eq!(seen, [0, 16, 32, 40], "once per strip and once at the end");
    assert!(out.len() < whole.len());

    let big = vec![0; MAX_SEGMENT_PAYLOAD + 1];
    let segments = [(1, big.as_slice())];
    let settings = Settings {
        pixels_per_inch: None,
        segments: &segments,
        ..settings
    };
    let mut out = Vec::new();
    let error = encode(&mut out, 40, 40, &rgba, &settings, &mut |_| {
        Ok::<_, Stepped>(())
    })
    .unwrap_err();
    assert!(matches!(error, Stepped::Jpeg(JpegError::Internal(_))));
    assert!(out.is_empty());
}

/// A `width` × `height` opaque RGBA frame of noise, which libjpeg cannot compress much: a few
/// rows of it fill libjpeg's output buffer, so an encode writes to its writer long before the end.
fn noise(width: u32, height: u32) -> Vec<u8> {
    let mut state = 0x2545_f491_u32;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for _ in 0..width as usize * height as usize {
        for _ in 0..3 {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            rgba.push(state as u8);
        }
        rgba.push(255);
    }
    rgba
}

/// Encode `rgba` through a session in bands whose sizes cycle through `sizes`, the last cut short
/// where the image ends; answers the file and the rows the step was called with.
fn streamed(
    width: u32,
    height: u32,
    rgba: &[u8],
    settings: &Settings<'_>,
    sizes: &[usize],
) -> (Vec<u8>, Vec<usize>) {
    let stride = width as usize * 4;
    let mut out = Vec::new();
    let mut steps = Vec::new();
    let mut encoder = Encoder::start(&mut out, width, height, settings).unwrap();
    let mut sizes = sizes.iter().cycle();
    let mut at = 0;
    while at < height as usize {
        let rows = (*sizes.next().unwrap()).min(height as usize - at);
        let band = &rgba[at * stride..(at + rows) * stride];
        encoder
            .write_rows(rows, band, &mut |done| {
                steps.push(done);
                Ok::<_, JpegError>(())
            })
            .unwrap();
        at += rows;
    }
    encoder.finish().unwrap();
    (out, steps)
}

/// Bands of 1, 7, 16 and 296 rows, the last of each cut short where the image ends, and bands of
/// mixed sizes encode exactly the bytes the whole frame does, header segments and ICC chunks
/// included, and the step runs at the same rows: libjpeg is handed the same strips whatever the
/// bands. 613 rows are a multiple of none of the sizes, and 61 columns of no MCU's width.
#[test]
fn streamed_bands_encode_the_bytes_the_whole_frame_does() {
    let (width, height) = (61, 613);
    let rgba = noise(width, height);
    let profile: Vec<u8> = (0..3000).map(|i| (i % 251) as u8).collect();
    let segments = [(1, b"Exif\0\0MM\0*".as_slice())];
    let settings = Settings {
        pixels_per_inch: None,
        quality: 90,
        chroma: (1, 1),
        segments: &segments,
        icc: Some(&profile),
    };
    let mut whole = Vec::new();
    let mut whole_steps = Vec::new();
    encode(&mut whole, width, height, &rgba, &settings, &mut |rows| {
        whole_steps.push(rows);
        Ok::<_, JpegError>(())
    })
    .unwrap();
    let strips: Vec<usize> = (0..height as usize).step_by(STRIP_ROWS).collect();
    assert_eq!(whole_steps, [strips.as_slice(), &[613]].concat());
    let (decoded_width, decoded_height, _) = decode(&whole).unwrap();
    assert_eq!((decoded_width, decoded_height), (width, height));
    for sizes in [&[1][..], &[7], &[16], &[296], &[1, 16, 7, 296, 3, 32]] {
        let (bytes, steps) = streamed(width, height, &rgba, &settings, sizes);
        assert!(bytes == whole, "bands of {sizes:?} encode other bytes");
        assert_eq!(steps, whole_steps, "bands of {sizes:?}");
    }
}

/// A writer whose bytes a test reads while an encode still holds it.
struct Shared(Rc<RefCell<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.borrow_mut().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A streamed encode checks its step before each strip, whatever the bands: an error from it
/// abandons the encode between two strips and is returned as it is. The writer then holds what
/// libjpeg had written of the strips before, the start of the whole file, and receives nothing
/// more: further rows and the trailer are refused. Abandoning an encode with `abort` after the same
/// strips leaves its writer the same.
#[test]
fn a_streamed_encode_cancels_between_strips_and_writes_nothing_more() {
    let (width, height) = (512, 200);
    let rgba = noise(width, height);
    let stride = width as usize * 4;
    let settings = Settings {
        pixels_per_inch: None,
        quality: 90,
        chroma: (1, 1),
        segments: &[],
        icc: None,
    };
    let mut whole = Vec::new();
    encode(&mut whole, width, height, &rgba, &settings, &mut |_| {
        Ok::<_, JpegError>(())
    })
    .unwrap();

    let written = Rc::new(RefCell::new(Vec::new()));
    let mut encoder =
        Encoder::start(Shared(Rc::clone(&written)), width, height, &settings).unwrap();
    let mut seen = Vec::new();
    let mut bands = rgba.chunks(7 * stride);
    let error = loop {
        let band = bands
            .next()
            .expect("the step stops the encode before the end");
        let result = encoder.write_rows(band.len() / stride, band, &mut |rows| {
            seen.push(rows);
            if rows >= 96 {
                Err(Stepped::Stop)
            } else {
                Ok(())
            }
        });
        if let Err(error) = result {
            break error;
        }
    };
    assert!(matches!(error, Stepped::Stop), "{error:?}");
    assert_eq!(seen, [0, 16, 32, 48, 64, 80, 96], "once before each strip");
    let at_cancel = written.borrow().clone();
    assert!(
        !at_cancel.is_empty() && at_cancel.len() < whole.len(),
        "{} of {} bytes were written before the cancel",
        at_cancel.len(),
        whole.len()
    );
    assert!(whole.starts_with(&at_cancel), "the start of the whole file");
    let more = encoder
        .write_rows(7, &rgba[..7 * stride], &mut |_| Ok::<_, Stepped>(()))
        .unwrap_err();
    assert!(
        matches!(more, Stepped::Jpeg(JpegError::Internal(_))),
        "{more:?}"
    );
    let trailer = encoder.finish().unwrap_err();
    assert!(matches!(trailer, JpegError::Internal(_)), "{trailer:?}");
    assert!(*written.borrow() == at_cancel, "nothing more was written");

    let aborted = Rc::new(RefCell::new(Vec::new()));
    let mut encoder =
        Encoder::start(Shared(Rc::clone(&aborted)), width, height, &settings).unwrap();
    for band in rgba[..96 * stride].chunks(7 * stride) {
        encoder
            .write_rows(band.len() / stride, band, &mut |_| Ok::<_, JpegError>(()))
            .unwrap();
    }
    encoder.abort();
    assert!(
        *aborted.borrow() == at_cancel,
        "abort leaves what the cancel left"
    );
}

/// A band that is not the rows it declares, shorter or longer, rows past the image's last and a
/// trailer before the last row are each refused by name before libjpeg sees them, never as a
/// libjpeg error or an abort of the process. A refused band leaves the session as it was, so the
/// file is still the whole frame's; a refused trailer abandons the file.
#[test]
fn a_short_band_is_an_error_not_an_abort() {
    const NOT_ITS_ROWS: &str = "jpeg encode: a band's pixels are not the rows it declares";
    const PAST_THE_END: &str = "jpeg encode: rows past the end of the image";
    let (width, height) = (40, 40);
    let rgba = noise(width, height);
    let stride = width as usize * 4;
    let settings = Settings {
        pixels_per_inch: None,
        quality: 90,
        chroma: (1, 1),
        segments: &[],
        icc: None,
    };
    let mut whole = Vec::new();
    encode(&mut whole, width, height, &rgba, &settings, &mut |_| {
        Ok::<_, JpegError>(())
    })
    .unwrap();
    let mut ok = |_: usize| Ok::<_, JpegError>(());
    let refusal = |error: JpegError| match error {
        JpegError::Internal(why) => why,
        other => panic!("refused by libjpeg rather than by name: {other:?}"),
    };

    let mut out = Vec::new();
    let mut encoder = Encoder::start(&mut out, width, height, &settings).unwrap();
    for (rows, band) in [
        (7, &rgba[..7 * stride - 1]),
        (7, &rgba[..6 * stride]),
        (6, &rgba[..7 * stride]),
        (1, &rgba[..3]),
    ] {
        let error = encoder.write_rows(rows, band, &mut ok).unwrap_err();
        assert_eq!(
            refusal(error),
            NOT_ITS_ROWS,
            "{rows} rows of {} bytes",
            band.len()
        );
    }
    encoder
        .write_rows(33, &rgba[..33 * stride], &mut ok)
        .unwrap();
    let beyond = vec![0; 8 * stride];
    let error = encoder.write_rows(8, &beyond, &mut ok).unwrap_err();
    assert_eq!(refusal(error), PAST_THE_END);
    encoder
        .write_rows(7, &rgba[33 * stride..], &mut ok)
        .unwrap();
    let error = encoder.write_rows(1, &rgba[..stride], &mut ok).unwrap_err();
    assert_eq!(refusal(error), PAST_THE_END, "the image is complete");
    encoder.finish().unwrap();
    assert!(out == whole, "the refused bands changed nothing");

    let mut early = Vec::new();
    let mut encoder = Encoder::start(&mut early, width, height, &settings).unwrap();
    encoder
        .write_rows(39, &rgba[..39 * stride], &mut ok)
        .unwrap();
    assert_eq!(
        refusal(encoder.finish().unwrap_err()),
        "jpeg encode: finished before the last row"
    );
    assert!(early.len() < whole.len() && whole.starts_with(&early));
}

/// How far the crate's decode is from the `image` crate's (zune-jpeg 0.5.15), which decoded
/// originals before it, and which warnings each decode went on past: per file, the mean and
/// largest absolute difference per channel, the share of pixels that differ at all and by more
/// than one and two codes, and the warnings accepted, or why the decode was refused. Every
/// committed s0 JPEG, plus the files named in `LUXFORGE_JPEG_COMPARE` (colon-separated). A
/// measurement, not a gate:
///
/// ```text
/// LUXFORGE_JPEG_COMPARE=/path/24mp.jpg:/path/photo.jpg \
///   cargo test --release -p luxforge-jpeg --lib tests::measure -- --ignored --nocapture
/// ```
#[test]
#[ignore = "a measurement, not a gate"]
fn measure_the_difference_from_the_previous_decoder() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/s0");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "jpg"))
        .collect();
    paths.sort();
    if let Ok(extra) = std::env::var("LUXFORGE_JPEG_COMPARE") {
        paths.extend(extra.split(':').map(std::path::PathBuf::from));
    }
    let mut rows = Vec::new();
    for path in paths {
        let bytes = std::fs::read(&path).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        accepted();
        let (width, height, rgba) = match decode(&bytes) {
            Ok(decoded) => decoded,
            Err(error) => {
                rows.push(json!({"file": name, "refused": error.to_string()}));
                continue;
            }
        };
        let warnings: Vec<_> = accepted()
            .into_iter()
            .map(|code| warnings::name(code).unwrap_or("unlisted"))
            .collect();
        let (zune_width, zune_height, rgb) = independent(&bytes);
        assert_eq!((width, height), (zune_width, zune_height), "{name}");
        let pixels = width as usize * height as usize;
        let mut sums = [0_u64; 3];
        let mut largest = [0_u8; 3];
        let (mut any, mut over_1, mut over_2) = (0_usize, 0_usize, 0_usize);
        for (a, z) in rgba.chunks_exact(4).zip(rgb.chunks_exact(3)) {
            let mut worst = 0;
            for channel in 0..3 {
                let difference = a[channel].abs_diff(z[channel]);
                sums[channel] += u64::from(difference);
                largest[channel] = largest[channel].max(difference);
                worst = worst.max(difference);
            }
            any += usize::from(worst > 0);
            over_1 += usize::from(worst > 1);
            over_2 += usize::from(worst > 2);
        }
        rows.push(json!({
            "file": name,
            "dimensions": [width, height],
            "accepted_warnings": warnings,
            "mean_abs_codes": sums.map(|sum| sum as f64 / pixels as f64),
            "max_abs_codes": largest,
            "share_changed": any as f64 / pixels as f64,
            "share_over_1": over_1 as f64 / pixels as f64,
            "share_over_2": over_2 as f64 / pixels as f64,
        }));
    }
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
}
