use super::*;
use crate::tests::{
    AFTER_APP0, LIMITS, Seeded, accepted, baseline, decode, first_scan, fixture, next_marker,
    pattern, scene, spliced,
};
use mozjpeg::{ColorSpace, Compress};
use mozjpeg_sys::{
    JWRN_BOGUS_PROGRESSION, JWRN_EXTRANEOUS_DATA, JWRN_HIT_MARKER, JWRN_HUFF_BAD_CODE,
    JWRN_JFIF_MAJOR, JWRN_MUST_RESYNC,
};

/// `region` of the `width`-wide RGBA frame `whole`.
fn cut(whole: &[u8], width: u32, region: Region) -> Vec<u8> {
    let stride = width as usize * 4;
    (region.y..region.y + region.height)
        .flat_map(|y| {
            let start = y as usize * stride + region.x as usize * 4;
            whole[start..start + region.width as usize * 4]
                .iter()
                .copied()
        })
        .collect()
}

/// `region` decoded through [`RegionDecoder`] in one call.
fn decode_region(bytes: &[u8], region: Region) -> Result<Vec<u8>, JpegError> {
    let mut decoder = RegionDecoder::new(bytes, LIMITS, region)?;
    assert_eq!(
        (decoder.width(), decoder.height()),
        (region.width, region.height)
    );
    let mut rows = vec![0; region.width as usize * region.height as usize * 4];
    decoder.read_rows(&mut rows)?;
    assert!(decoder.session.is_none(), "the last row ends the session");
    Ok(rows)
}

/// A rectangle at `(x, y)` of up to `width` × `height`, clipped to a `frame`; `None` when nothing
/// is left.
fn clipped(frame: (u32, u32), x: u32, y: u32, width: u32, height: u32) -> Option<Region> {
    (x < frame.0 && y < frame.1).then(|| Region {
        x,
        y,
        width: width.min(frame.0 - x),
        height: height.min(frame.1 - y),
    })
}

/// The rectangles every frame is cut at: the whole frame, a pixel and a block at every corner,
/// strips along every edge, a single row and column, offsets and sizes on and off the MCU grid
/// (8 and 16 px) and straddling it, then `random` rectangles from a seeded generator.
fn rectangles(frame: (u32, u32), random: usize, seed: u64) -> Vec<Region> {
    let (w, h) = frame;
    let (right, bottom) = (w - 1, h - 1);
    let mut regions: Vec<Region> = [
        (0, 0, w, h),
        (0, 0, 1, 1),
        (right, 0, 1, 1),
        (0, bottom, 1, 1),
        (right, bottom, 1, 1),
        (0, 0, 23, 19),
        (w.saturating_sub(23), 0, 23, 19),
        (0, h.saturating_sub(19), 23, 19),
        (w.saturating_sub(23), h.saturating_sub(19), 23, 19),
        (0, 0, w, 9),
        (0, h.saturating_sub(9), w, 9),
        (0, 0, 9, h),
        (w.saturating_sub(9), 0, 9, h),
        (0, h / 2, w, 1),
        (w / 2, 0, 1, h),
        (5, 3, 37, 21),
        (17, 9, 16, 16),
        (16, 16, 13, 7),
        (16, 8, 1, 1),
        (15, 15, 2, 2),
        (31, 15, 3, 3),
        (7, 7, 2, 18),
        (8, 16, 32, 32),
        (33, 1, 1, 30),
    ]
    .into_iter()
    .filter_map(|(x, y, width, height)| clipped(frame, x, y, width, height))
    .collect();
    let mut seeded = Seeded(seed);
    for _ in 0..random {
        let (x, y) = (seeded.below(w), seeded.below(h));
        let width = 1 + seeded.below(w - x);
        let height = 1 + seeded.below(h - y);
        regions.push(Region {
            x,
            y,
            width,
            height,
        });
    }
    regions
}

/// `scene` encoded progressively (libjpeg's simple progression script) at `chroma` sampling, or
/// greyscale when `chroma` is `None`.
fn progressive(width: u32, height: u32, chroma: Option<(u8, u8)>) -> Vec<u8> {
    let rgba = scene(width, height);
    let (space, pixels): (ColorSpace, Vec<u8>) = match chroma {
        Some(_) => (
            ColorSpace::JCS_RGB,
            rgba.chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect(),
        ),
        None => (
            ColorSpace::JCS_GRAYSCALE,
            rgba.chunks_exact(4).map(|p| p[1]).collect(),
        ),
    };
    let mut compress = Compress::new(space);
    compress.set_fastest_defaults();
    compress.set_size(width as usize, height as usize);
    compress.set_quality(88.0);
    if let Some(sampling) = chroma {
        compress.set_chroma_sampling_pixel_sizes(sampling, sampling);
    }
    compress.set_progressive_mode();
    let mut started = compress.start_compress(Vec::new()).unwrap();
    started.write_scanlines(&pixels).unwrap();
    started.finish().unwrap()
}

/// `scene` in greyscale, baseline.
fn greyscale(width: u32, height: u32) -> Vec<u8> {
    let pixels: Vec<u8> = scene(width, height).chunks_exact(4).map(|p| p[0]).collect();
    let mut compress = Compress::new(ColorSpace::JCS_GRAYSCALE);
    compress.set_fastest_defaults();
    compress.set_size(width as usize, height as usize);
    compress.set_quality(88.0);
    let mut started = compress.start_compress(Vec::new()).unwrap();
    started.write_scanlines(&pixels).unwrap();
    started.finish().unwrap()
}

/// The offsets of every SOS marker: the first scan's, then each marker after a scan's data, with
/// the segments between scans passed over by their lengths.
fn scans(bytes: &[u8]) -> Vec<usize> {
    let (first, mut at) = first_scan(bytes);
    let mut found = vec![first];
    loop {
        let marker = next_marker(bytes, at);
        let code = bytes[marker + 1];
        if code == 0xd9 {
            return found;
        }
        if code == 0xda {
            found.push(marker);
        }
        at = marker + 2 + usize::from(u16::from_be_bytes([bytes[marker + 2], bytes[marker + 3]]));
    }
}

/// A progressive file at `chroma` sampling without its last `dropped` scans, refinements: its
/// coefficients are left partly unrefined, so libjpeg smooths blocks from their neighbours' DC
/// values (`jdcoefct.c`), reading two blocks each way. The simple progression script ends with
/// the luma refinement, before it the chroma ones, so dropping three leaves every component
/// unrefined.
fn unrefined(width: u32, height: u32, chroma: (u8, u8), dropped: usize) -> Vec<u8> {
    let bytes = progressive(width, height, Some(chroma));
    let scans = scans(&bytes);
    let first_dropped = scans[scans.len() - dropped];
    spliced(&bytes, first_dropped, bytes.len() - 2 - first_dropped, &[])
}

/// The unrefined files the corpus holds, as `(chroma, dropped)`.
const UNREFINED: [((u8, u8), usize); 3] = [((2, 2), 1), ((2, 2), 3), ((1, 1), 3)];

/// Every layout the crate supports: the committed fixtures, and frames generated at every chroma
/// sampling, baseline and progressive, at sizes off the MCU grid.
fn corpus() -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = [
        "orientation-1.jpg",
        "orientation-6.jpg",
        "portrait.jpg",
        "srgb.jpg",
        "greyscale.jpg",
        "jpeg-scan-420-interleaved.jpg",
        "jpeg-scan-420-noninterleaved.jpg",
        "jpeg-scan-422-interleaved.jpg",
        "jpeg-scan-422-noninterleaved.jpg",
        "jpeg-scan-444-interleaved.jpg",
        "jpeg-scan-444-noninterleaved.jpg",
        "jpeg-restart.jpg",
    ]
    .into_iter()
    .map(|name| (name.to_owned(), fixture(name)))
    .collect();
    files.push(("progressive twin 61x37".into(), pattern(61, 37, true)));
    for (width, height) in [(203, 117), (130, 69)] {
        for chroma in [(1, 1), (2, 1), (1, 2), (2, 2), (4, 1)] {
            files.push((
                format!("baseline {width}x{height} {chroma:?}"),
                baseline(width, height, chroma),
            ));
        }
        for chroma in [Some((1, 1)), Some((2, 1)), Some((2, 2)), None] {
            files.push((
                format!("progressive {width}x{height} {chroma:?}"),
                progressive(width, height, chroma),
            ));
        }
        files.push((
            format!("greyscale {width}x{height}"),
            greyscale(width, height),
        ));
        for (chroma, dropped) in UNREFINED {
            files.push((
                format!("progressive {width}x{height} {chroma:?} without {dropped} scans"),
                unrefined(width, height, chroma, dropped),
            ));
        }
    }
    files
}

/// Exactness, the acceptance test: every region equals the same rectangle cut from the full
/// decode, byte for byte, over every layout and every kind of rectangle.
#[test]
fn a_region_is_the_same_rectangle_cut_from_the_full_decode() {
    let mut checked = 0;
    for (index, (name, bytes)) in corpus().into_iter().enumerate() {
        let (width, height, whole) = decode(&bytes).unwrap();
        for region in rectangles((width, height), 40, 0x9e37_79b9 + index as u64) {
            let actual = decode_region(&bytes, region)
                .unwrap_or_else(|error| panic!("{name} {region:?}: {error}"));
            assert!(
                actual == cut(&whole, width, region),
                "{name} {region:?} differs from the full decode"
            );
            checked += 1;
        }
    }
    assert!(accepted().is_empty(), "no generated file warns");
    println!("{checked} regions matched");
}

/// Every single column and every single row of every frame, alone: each column is both
/// a region's first and last, at every offset from the iMCU grid, which is where a crop's edges
/// differ from a full decode; each row is a skip to every offset from the iMCU rows.
#[test]
fn every_column_and_every_row_alone_is_the_same_as_the_full_decode() {
    let mut checked = 0;
    for (name, bytes) in corpus() {
        let (width, height, whole) = decode(&bytes).unwrap();
        let columns = (0..width).map(|x| Region {
            x,
            y: 0,
            width: 1,
            height,
        });
        let rows = (0..height).map(|y| Region {
            x: 0,
            y,
            width,
            height: 1,
        });
        for region in columns.chain(rows) {
            let actual = decode_region(&bytes, region)
                .unwrap_or_else(|error| panic!("{name} {region:?}: {error}"));
            assert!(
                actual == cut(&whole, width, region),
                "{name} {region:?} differs from the full decode"
            );
            checked += 1;
        }
    }
    println!("{checked} columns and rows matched");
}

/// The unrefined progressive files do take libjpeg's block smoothing, which reads neighbouring
/// blocks, so the exactness above covers it: they decode differently with smoothing turned off.
#[test]
fn the_unrefined_progression_is_smoothed() {
    for (width, height) in [(203, 117), (130, 69)] {
        for (chroma, dropped) in UNREFINED {
            let bytes = unrefined(width, height, chroma, dropped);
            let (_, _, smoothed) = decode(&bytes).unwrap();
            let mut flat = mozjpeg::Decompress::new_mem(&bytes).unwrap();
            flat.do_block_smoothing(false);
            let mut flat = flat.rgba().unwrap();
            let flat: Vec<u8> = flat.read_scanlines().unwrap();
            assert!(flat != smoothed, "{width}x{height} {chroma:?} {dropped}");
        }
    }
}

#[test]
fn a_region_reads_in_any_whole_strips_and_never_past_its_end() {
    let bytes = fixture("orientation-1.jpg");
    let (width, _, whole) = decode(&bytes).unwrap();
    let region = Region {
        x: 41,
        y: 29,
        width: 77,
        height: 50,
    };
    let stride = 77 * 4;
    let mut decoder = RegionDecoder::new(&bytes, LIMITS, region).unwrap();
    // Asking for no rows asks libjpeg for nothing, before, between and after the strips.
    decoder.read_rows(&mut []).unwrap();
    assert!(decoder.window.is_none(), "nothing started");
    let mut strips = Vec::new();
    for rows in [1, 16, 0, 7, 26] {
        let mut strip = vec![0; rows * stride];
        decoder.read_rows(&mut strip).unwrap();
        strips.extend(strip);
    }
    assert!(strips == cut(&whole, width, region));
    decoder.read_rows(&mut []).unwrap();
    let past = decoder.read_rows(&mut vec![0; stride]).unwrap_err();
    assert!(matches!(past, JpegError::Internal(_)), "{past:?}");

    let mut decoder = RegionDecoder::new(&bytes, LIMITS, region).unwrap();
    for wrong in [stride + 4, stride - 4, stride * 51] {
        let error = decoder.read_rows(&mut vec![0; wrong]).unwrap_err();
        assert!(
            matches!(error, JpegError::Internal(_)),
            "{wrong}: {error:?}"
        );
    }
    // A refused request reaches nothing, so the decode goes on.
    let mut rows = vec![0; stride * 50];
    decoder.read_rows(&mut rows).unwrap();
    assert!(rows == cut(&whole, width, region));
}

/// The caller's rows are the rectangle's size; the one buffer the decode allocates is a row of the
/// window, at most four iMCU columns wider than the region, however large the frame.
#[test]
fn a_region_allocates_its_rectangle_and_one_window_row() {
    let bytes = baseline(1203, 817, (2, 2));
    let (width, _, whole) = decode(&bytes).unwrap();
    for region in [
        Region {
            x: 600,
            y: 400,
            width: 1,
            height: 1,
        },
        Region {
            x: 333,
            y: 555,
            width: 97,
            height: 61,
        },
        Region {
            x: 0,
            y: 0,
            width: 64,
            height: 64,
        },
        Region {
            x: 1200,
            y: 800,
            width: 3,
            height: 17,
        },
    ] {
        let mut decoder = RegionDecoder::new(&bytes, LIMITS, region).unwrap();
        let mut rows = vec![0; region.width as usize * region.height as usize * 4];
        decoder.read_rows(&mut rows).unwrap();
        assert!(rows == cut(&whole, width, region), "{region:?}");
        let window = decoder.window.as_ref().unwrap();
        let imcu = 16;
        assert!(
            window.row.len() <= (region.width as usize + 5 * imcu) * 4,
            "{region:?}: a window row of {} bytes",
            window.row.len()
        );
        assert!(decoder.session.is_none());
    }
}

/// An empty rectangle, one past any edge and one whose end overflows are refused as
/// [`JpegError::Internal`] before libjpeg reads the file: here a frame header libjpeg itself
/// refuses (its SOF segment stops after the component count) while the container walk reads it.
/// A frame past the limits or in another colour space is refused as a full decode refuses it.
#[test]
fn regions_outside_the_frame_are_refused_before_libjpeg_reads_the_file() {
    let mut short_sof = vec![0xff, 0xd8, 0xff, 0xc0, 0, 8, 8, 0, 32, 0, 48, 3];
    short_sof.extend([0xff, 0xda, 0, 2, 0xff, 0xd9]);
    let inside = Region {
        x: 0,
        y: 0,
        width: 48,
        height: 32,
    };
    let error = RegionDecoder::new(&short_sof, LIMITS, inside)
        .err()
        .unwrap();
    assert!(matches!(error, JpegError::Undecodable), "{error:?}");
    for outside in [
        Region { width: 0, ..inside },
        Region {
            height: 0,
            ..inside
        },
        Region { x: 1, ..inside },
        Region { y: 1, ..inside },
        Region {
            x: 47,
            width: 2,
            ..inside
        },
        Region {
            x: u32::MAX,
            width: 2,
            ..inside
        },
        Region {
            y: u32::MAX,
            height: u32::MAX,
            ..inside
        },
    ] {
        let error = RegionDecoder::new(&short_sof, LIMITS, outside)
            .err()
            .unwrap();
        assert!(
            matches!(error, JpegError::Internal(_)),
            "{outside:?}: {error:?}"
        );
    }

    let small = Limits {
        max_side: 479,
        max_pixels: 64_000_000,
    };
    let pixel = Region {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let bytes = fixture("orientation-1.jpg");
    let wide = RegionDecoder::new(&bytes, small, pixel).err().unwrap();
    assert!(matches!(wide, JpegError::Dimensions), "{wide:?}");
    let oversized = RegionDecoder::new(&fixture("oversized.jpg"), LIMITS, pixel)
        .err()
        .unwrap();
    assert!(matches!(oversized, JpegError::Dimensions), "{oversized:?}");
    let cmyk = RegionDecoder::new(&fixture("cmyk.jpg"), LIMITS, pixel)
        .err()
        .unwrap();
    assert!(matches!(cmyk, JpegError::ColourSpace), "{cmyk:?}");
}

/// The same error a full decode gives, for `bytes` cut at `region`.
fn same_refusal(bytes: &[u8], region: Region) -> JpegError {
    let full = decode(bytes)
        .map(|_| ())
        .expect_err("the full decode refuses");
    let cut = decode_region(bytes, region)
        .map(|_| ())
        .expect_err("the region refuses");
    assert_eq!(
        format!("{full:?}"),
        format!("{cut:?}"),
        "{region:?} refuses as the full decode does"
    );
    cut
}

/// Corrupt data inside the region, or in the rows above it that a single-scan decode still
/// entropy-decodes, refuses the region with the warning a full decode refuses with; so does a
/// broken restart sequence above it, a progressive file with a scan missing (read before the
/// first row whatever the region) and a libjpeg fatal error. A decode after one works.
#[test]
fn corrupt_data_the_region_needs_refuses_it_as_a_full_decode_does() {
    let clean = pattern(61, 37, false);
    let (_, scan) = first_scan(&clean);
    let end = clean.len() - 2;
    let middle = scan + (end - scan) / 2;
    let bottom = Region {
        x: 13,
        y: 30,
        width: 40,
        height: 7,
    };
    let whole = Region {
        x: 0,
        y: 0,
        width: 61,
        height: 37,
    };

    let ones = [0xff, 0x00].repeat(8);
    let bad = spliced(&clean, middle, ones.len(), &ones);
    for region in [bottom, whole] {
        let error = same_refusal(&bad, region);
        assert!(matches!(error, JpegError::Corrupt(JWRN_HUFF_BAD_CODE)));
    }
    let cut = spliced(&clean, middle, end - middle, &[]);
    for region in [bottom, whole] {
        let error = same_refusal(&cut, region);
        assert!(matches!(error, JpegError::Corrupt(JWRN_HIT_MARKER)));
    }

    let restarts = fixture("jpeg-restart.jpg");
    let (_, restart_scan) = first_scan(&restarts);
    let first_restart = (restart_scan..restarts.len())
        .find(|&i| restarts[i] == 0xff && restarts[i + 1] == 0xd0)
        .unwrap();
    let mut resync = restarts.clone();
    resync[first_restart + 1] = 0xd5;
    let below = Region {
        x: 20,
        y: 20,
        width: 20,
        height: 12,
    };
    let error = same_refusal(&resync, below);
    assert!(matches!(error, JpegError::Corrupt(JWRN_MUST_RESYNC)));

    let progressive = pattern(61, 37, true);
    let (dc, dc_data) = first_scan(&progressive);
    let no_dc = spliced(
        &progressive,
        dc,
        next_marker(&progressive, dc_data) - dc,
        &[],
    );
    let top_left = Region {
        x: 0,
        y: 0,
        width: 3,
        height: 3,
    };
    let error = same_refusal(&no_dc, top_left);
    assert!(matches!(error, JpegError::Corrupt(JWRN_BOGUS_PROGRESSION)));

    let valid = fixture("orientation-1.jpg");
    let (sos, _) = first_scan(&valid);
    let mut broken = valid.clone();
    broken[sos + 5] = 0x7f;
    let error = same_refusal(&broken, top_left);
    assert!(matches!(error, JpegError::Undecodable));

    assert!(accepted().is_empty(), "nothing was let through");
    assert!(decode_region(&clean, whole).is_ok());
}

/// A single-scan decode stops after the rows the region needs, so corrupt data below them and
/// bytes after the scan are never read: the region above decodes to the clean file's pixels,
/// where the full decode refuses. A multi-scan file's scans are all read before the first row, so
/// there the same trailing bytes refuse the region too.
#[test]
fn data_after_what_the_region_needs_is_not_read() {
    let clean = baseline(203, 117, (2, 2));
    let (width, _, whole) = decode(&clean).unwrap();
    let (_, scan) = first_scan(&clean);
    let end = clean.len() - 2;
    let top = Region {
        x: 30,
        y: 2,
        width: 100,
        height: 10,
    };
    let ones = [0xff, 0x00].repeat(8);
    let late = end - 40;
    assert!(
        late > scan + (end - scan) * 3 / 4,
        "in the last quarter of the scan"
    );
    let bad = spliced(&clean, late, ones.len(), &ones);
    assert!(matches!(
        decode(&bad),
        Err(JpegError::Corrupt(JWRN_HUFF_BAD_CODE))
    ));
    assert!(decode_region(&bad, top).unwrap() == cut(&whole, width, top));

    let trailing = spliced(&clean, end, 0, &[0x55; 32]);
    assert!(matches!(
        decode(&trailing),
        Err(JpegError::Corrupt(JWRN_EXTRANEOUS_DATA))
    ));
    let last_row = Region {
        x: 0,
        y: 116,
        width: 203,
        height: 1,
    };
    for region in [top, last_row] {
        assert!(decode_region(&trailing, region).unwrap() == cut(&whole, width, region));
    }

    let progressive = pattern(61, 37, true);
    let end = progressive.len() - 2;
    let trailing = spliced(&progressive, end, 0, &[0x55; 32]);
    let error = same_refusal(
        &trailing,
        Region {
            x: 0,
            y: 0,
            width: 1,
            height: 1,
        },
    );
    assert!(matches!(error, JpegError::Corrupt(JWRN_EXTRANEOUS_DATA)));
    assert!(accepted().is_empty());
}

/// Harmless irregularities in the header are let through as a full decode lets them through, and
/// the region's pixels are the clean file's.
#[test]
fn a_harmless_warning_decodes_the_same_region() {
    let clean = pattern(61, 37, false);
    let (width, _, whole) = decode(&clean).unwrap();
    let region = Region {
        x: 9,
        y: 17,
        width: 30,
        height: 11,
    };
    let expected = cut(&whole, width, region);
    let extraneous = spliced(&clean, AFTER_APP0, 0, &[0x12, 0x34, 0xff, 0x00, 0x56]);
    assert!(decode_region(&extraneous, region).unwrap() == expected);
    assert_eq!(accepted(), [JWRN_EXTRANEOUS_DATA]);
    let mut jfif_2 = clean.clone();
    jfif_2[11] = 2;
    assert!(decode_region(&jfif_2, region).unwrap() == expected);
    assert_eq!(accepted(), [JWRN_JFIF_MAJOR]);
}

/// A file cut short is refused by the container walk, as a full decode refuses it; and no prefix
/// of a file, closed with an EOI so that libjpeg reads it, panics or aborts a region decode: each
/// either decodes or is refused with an error.
#[test]
fn truncation_is_refused_and_no_prefix_panics() {
    let pixel = Region {
        x: 0,
        y: 0,
        width: 1,
        height: 1,
    };
    let truncated = RegionDecoder::new(&fixture("truncated.jpg"), LIMITS, pixel)
        .err()
        .unwrap();
    assert!(
        matches!(truncated, JpegError::Malformed(_)),
        "{truncated:?}"
    );
    let valid = fixture("orientation-1.jpg");
    let (_, scan) = first_scan(&valid);
    let cut = &valid[..scan + (valid.len() - scan) / 2];
    assert!(matches!(
        RegionDecoder::new(cut, LIMITS, pixel),
        Err(JpegError::Malformed(_))
    ));

    let mut outcomes = [0_usize; 2];
    for (bytes, step) in [
        (fixture("jpeg-scan-420-noninterleaved.jpg"), 1),
        (fixture("jpeg-restart.jpg"), 1),
        (pattern(61, 37, true), 1),
        (valid, 7),
    ] {
        let (width, height) = {
            let header = crate::header(&bytes).unwrap();
            (header.width, header.height)
        };
        let region = Region {
            x: width / 3,
            y: height / 3,
            width: width / 2,
            height: height / 2,
        };
        for end in (2..bytes.len() - 2).step_by(step) {
            let prefix = [&bytes[..end], &[0xff, 0xd9]].concat();
            let outcome = decode_region(&prefix, region);
            outcomes[usize::from(outcome.is_ok())] += 1;
        }
    }
    accepted();
    println!("prefixes refused, decoded: {outcomes:?}");
    assert!(outcomes[0] > 0);
}
