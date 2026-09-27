use super::*;
use crate::ErrorKind;
use image::{ImageDecoder, codecs::jpeg::JpegDecoder};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{io::Cursor, path::Path};

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

/// The whole image decoded through the adapter, unoriented, as RGBA.
fn decode(bytes: &[u8]) -> Result<(u32, u32, Vec<u8>), Error> {
    let mut decoder = Decoder::new(bytes, LIMITS)?;
    let (width, height) = (decoder.width(), decoder.height());
    let mut rgba = vec![0; width as usize * height as usize * 4];
    decoder.read_rows(&mut rgba)?;
    decoder.finish()?;
    Ok((width, height, rgba))
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

/// The byte offset of the first scan's entropy-coded data.
fn scan_start(bytes: &[u8]) -> usize {
    let mut i = 2;
    loop {
        let marker = bytes[i + 1];
        let size = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        i += 2 + size;
        if marker == 0xda {
            return i;
        }
    }
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
}

/// A progressive file (SOF2) decodes to exactly the bytes of the baseline file holding the same
/// quantized coefficients, at 4:2:0: libjpeg's simple progression script refines every
/// coefficient to full precision, and both decode through the same IDCT and upsampling.
#[test]
fn a_progressive_file_decodes_as_its_baseline_twin() {
    let (width, height) = (61_usize, 37_usize);
    let rgb: Vec<u8> = (0..width * height)
        .flat_map(|i| {
            let (x, y) = (i % width, i / width);
            [(x * 4) as u8, (y * 7) as u8, ((x ^ y) * 3) as u8]
        })
        .collect();
    let encode_as = |progressive: bool| {
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
    };
    let (baseline, progressive) = (encode_as(false), encode_as(true));
    assert!(progressive.windows(2).any(|pair| pair == [0xff, 0xc2]));
    let (_, _, expected) = decode(&baseline).unwrap();
    let (_, _, actual) = decode(&progressive).unwrap();
    assert!(actual == expected);
}

#[test]
fn colour_spaces_and_limits_are_checked_before_pixels() {
    let cmyk = Decoder::new(&fixture("cmyk.jpg"), LIMITS).err().unwrap();
    assert_eq!(cmyk.kind, ErrorKind::UnsupportedColor);
    let small = Limits {
        max_side: 479,
        max_pixels: 64_000_000,
    };
    let wide = Decoder::new(&fixture("orientation-1.jpg"), small)
        .err()
        .unwrap();
    assert_eq!(wide.kind, ErrorKind::ResourceLimit);
    let few = Limits {
        max_side: 16384,
        max_pixels: 480 * 320 - 1,
    };
    let many = Decoder::new(&fixture("orientation-1.jpg"), few)
        .err()
        .unwrap();
    assert_eq!(many.kind, ErrorKind::ResourceLimit);
}

/// libjpeg warns and fills with grey where the data is cut short; the adapter makes each such
/// warning a decode error: data ending early, data cut by a marker, and a file whose EOI arrives
/// mid-scan. A decode after a failed one works, so nothing a failure left behind is reused.
#[test]
fn corrupt_or_truncated_data_is_an_error_not_grey() {
    let valid = fixture("orientation-1.jpg");
    let scan = scan_start(&valid);
    let middle = scan + (valid.len() - scan) / 2;
    let cut = &valid[..middle];
    let mut cut_by_eoi = cut.to_vec();
    cut_by_eoi.extend([0xff, 0xd9]);
    let mut cut_by_marker = cut.to_vec();
    cut_by_marker.extend([0xff, 0xd0]);
    cut_by_marker.extend(&valid[middle..]);
    for (what, bytes) in [
        ("ends early", cut.to_vec()),
        ("EOI mid-scan", cut_by_eoi),
        (
            "a restart marker in a stream without restarts",
            cut_by_marker,
        ),
    ] {
        let error = decode(&bytes).err().unwrap_or_else(|| panic!("{what}"));
        assert_eq!(error.kind, ErrorKind::Decode, "{what}: {error:?}");
        assert_eq!(error.detail, "corrupt or truncated JPEG data", "{what}");
    }
    assert!(decode(&valid).is_ok());
}

/// A libjpeg fatal error (here a scan naming a component the frame does not have) is an error and
/// never an abort, in the header and in the scan alike.
#[test]
fn a_libjpeg_fatal_error_is_an_error_not_an_abort() {
    let valid = fixture("orientation-1.jpg");
    // The SOS payload ends where the scan data starts; its first bytes are the component count
    // and the first component's selector.
    let mut i = 2;
    let sos = loop {
        let size = usize::from(u16::from_be_bytes([valid[i + 2], valid[i + 3]]));
        if valid[i + 1] == 0xda {
            break i + 4;
        }
        i += 2 + size;
    };
    let mut broken = valid.clone();
    broken[sos + 1] = 0x7f;
    let error = decode(&broken).err().unwrap();
    assert_eq!(error.kind, ErrorKind::Decode, "{error:?}");
    assert_eq!(error.detail, "JPEG data libjpeg cannot decode");
    assert_eq!(
        Decoder::new(b"not a jpeg", LIMITS).err().unwrap().kind,
        ErrorKind::Decode
    );
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
    let past = decoder.read_rows(&mut vec![0; stride]).unwrap_err();
    assert_eq!(past.kind, ErrorKind::Internal);
    assert_eq!(
        decoder.read_rows(&mut [0; 3]).unwrap_err().kind,
        ErrorKind::Internal
    );
    decoder.finish().unwrap();

    let early = Decoder::new(&bytes, LIMITS).unwrap();
    assert_eq!(early.finish().unwrap_err().kind, ErrorKind::Internal);
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
    let profile = icc_profile(
        [&third, &other, &first, &second]
            .into_iter()
            .map(Vec::as_slice),
    )
    .unwrap();
    assert_eq!(profile.as_deref(), Some(b"abcdefg".as_slice()));
    assert_eq!(
        icc_profile([other.as_slice()].into_iter()).unwrap(),
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
        let error = icc_profile(segments.iter().map(Vec::as_slice)).unwrap_err();
        assert_eq!(error.kind, ErrorKind::UnsupportedProfile, "{what}");
    }
    let big = vec![0; MAX_SEGMENT_PAYLOAD - 14];
    let chunks: Vec<_> = (1..=17).map(|n| icc_segment(n, 17, &big)).collect();
    let error = icc_profile(chunks.iter().map(Vec::as_slice)).unwrap_err();
    assert_eq!(error.kind, ErrorKind::UnsupportedProfile);
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

/// A step error abandons an encode and is returned unchanged, after the rows written so far; a
/// segment too large for one marker is refused before anything is written.
#[test]
fn an_encode_stops_where_its_step_says() {
    let rgba = vec![128; 40 * 40 * 4];
    let settings = Settings {
        quality: 90,
        chroma: (1, 1),
        segments: &[],
    };
    let mut seen = Vec::new();
    let mut out = Vec::new();
    let error = encode(&mut out, 40, 40, &rgba, &settings, &mut |rows| {
        seen.push(rows);
        if rows >= 32 {
            Err(Error::cancelled("stop"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(seen, [0, 16, 32]);

    let mut seen = Vec::new();
    let mut whole = Vec::new();
    encode(&mut whole, 40, 40, &rgba, &settings, &mut |rows| {
        seen.push(rows);
        Ok(())
    })
    .unwrap();
    assert_eq!(seen, [0, 16, 32, 40], "once per strip and once at the end");
    assert!(out.len() < whole.len());

    let big = vec![0; MAX_SEGMENT_PAYLOAD + 1];
    let segments = [(1, big.as_slice())];
    let settings = Settings {
        segments: &segments,
        ..settings
    };
    let mut out = Vec::new();
    let error = encode(&mut out, 40, 40, &rgba, &settings, &mut |_| Ok(())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Internal);
    assert!(out.is_empty());
}

/// How far the adapter's decode is from the `image` crate's (zune-jpeg 0.5.15), which decoded
/// originals before it: per file, the mean and largest absolute difference per channel and the
/// share of pixels that differ at all and by more than one and two codes. Every committed s0 JPEG
/// both decode, plus the files named in `LUXFORGE_JPEG_COMPARE` (colon-separated). A measurement,
/// not a gate:
///
/// ```text
/// LUXFORGE_JPEG_COMPARE=/path/24mp.jpg:/path/photo.jpg \
///   cargo test --release -p luxforge-core --lib jpeg::tests::measure -- --ignored --nocapture
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
        let Ok((width, height, rgba)) = decode(&bytes) else {
            rows.push(json!({"file": name, "adapter": "refused"}));
            continue;
        };
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
            "mean_abs_codes": sums.map(|sum| sum as f64 / pixels as f64),
            "max_abs_codes": largest,
            "share_changed": any as f64 / pixels as f64,
            "share_over_1": over_1 as f64 / pixels as f64,
            "share_over_2": over_2 as f64 / pixels as f64,
        }));
    }
    println!("{}", serde_json::to_string_pretty(&rows).unwrap());
}
