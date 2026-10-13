use super::*;
use crate::{
    ErrorKind, Raster, SnapshotId,
    export::encode::{Chroma, JpegOptions, encode_jpeg, encode_jpeg_limited},
};
use std::cell::{Cell, RefCell};

/// An encode whose file is `size(quality)` bytes, recording the qualities it was asked for.
fn sized<'a>(size: impl Fn(u8) -> usize + 'a, tried: &'a RefCell<Vec<u8>>) -> Box<Attempt<'a>> {
    Box::new(move |quality, progress| {
        tried.borrow_mut().push(quality);
        progress(0.5);
        progress(1.0);
        Ok(Some(vec![quality; size(quality)]))
    })
}

fn run(
    limit: u64,
    quality: u8,
    size: impl Fn(u8) -> usize,
) -> (Result<Limited, Error>, Vec<u8>, Vec<LimitProgress>) {
    let tried = RefCell::new(Vec::new());
    let mut reports = Vec::new();
    let result = search(
        limit,
        quality,
        &mut *sized(size, &tried),
        &mut |report| reports.push(report),
        &|| Ok(()),
    );
    (result, tried.into_inner(), reports)
}

/// For every requested quality and every limit between the smallest and the largest file, a search
/// whose file grows with quality answers the exact highest quality that fits, at most the one
/// asked for, in at most eight encodes, the requested one first; its file is that quality's.
#[test]
fn the_search_finds_the_highest_fitting_quality_in_eight_encodes() {
    let size = |quality: u8| 10 + usize::from(quality) * usize::from(quality) / 4;
    for requested in 1..=100u8 {
        let lowest = MIN_QUALITY.min(requested);
        // Each quality's own size and one byte less, and one byte more than the largest.
        let limits = (lowest..=requested)
            .flat_map(|quality| [size(quality) as u64 - 1, size(quality) as u64])
            .filter(|&limit| limit >= size(lowest) as u64)
            .chain([size(requested) as u64 + 1]);
        for limit in limits {
            let (result, tried, _) = run(limit, requested, size);
            let found = result.unwrap();
            let expected = (lowest..=requested)
                .rev()
                .find(|&quality| size(quality) as u64 <= limit)
                .unwrap();
            assert_eq!(
                found.quality, expected,
                "{requested} under {limit}: {tried:?}"
            );
            assert_eq!(found.bytes, vec![expected; size(expected)]);
            assert_eq!(found.attempts as usize, tried.len());
            assert!(
                found.attempts <= MAX_ATTEMPTS,
                "{requested} under {limit}: {tried:?}"
            );
            assert_eq!(tried[0], requested);
        }
    }
}

/// A requested quality that fits is the answer after one encode.
#[test]
fn a_fitting_request_takes_one_encode() {
    let (result, tried, reports) = run(1_000_000, 85, |quality| usize::from(quality) * 10);
    let found = result.unwrap();
    assert_eq!((found.quality, found.attempts), (85, 1));
    assert_eq!(tried, [85]);
    assert_eq!(reports.last().unwrap().fraction, 1.0);
}

/// When nothing fits at quality 10 the search refuses with `limit-unreachable`, naming the limit
/// and the quality, after trying 10 last, and answers no bytes. A request below 10 tries only
/// itself.
#[test]
fn an_unreachable_limit_is_refused_after_trying_the_lowest_quality() {
    for (requested, lowest) in [(100, 10), (60, 10), (10, 10), (5, 5)] {
        let (result, tried, _) = run(50, requested, |quality| 100 + usize::from(quality));
        let error = result.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Validation);
        let data = error.data.as_deref().unwrap();
        assert_eq!(data["code"], LIMIT_UNREACHABLE);
        assert_eq!(data["limit_bytes"], 50);
        assert_eq!(data["quality"], lowest);
        assert_eq!(tried.last(), Some(&lowest), "{tried:?}");
        assert!(tried.len() as u32 <= MAX_ATTEMPTS, "{tried:?}");
    }
}

/// An encode that stops because its file passed the limit (`None`) is an attempt that did not
/// fit.
#[test]
fn an_encode_stopped_at_the_limit_does_not_fit() {
    let tried = RefCell::new(Vec::new());
    let mut encode = |quality: u8, _: &mut dyn FnMut(f64)| {
        tried.borrow_mut().push(quality);
        Ok((quality <= 40).then(|| vec![0; 10]))
    };
    let found = search(100, 90, &mut encode, &mut |_| {}, &|| Ok(())).unwrap();
    assert_eq!(found.quality, 40, "{:?}", tried.borrow());
}

/// The cancel is checked before every attempt: a cancel after two attempts ends the search with
/// its error before a third encode, and an error from an encode ends it as it is.
#[test]
fn the_search_stops_between_attempts() {
    let checks = Cell::new(0);
    let tried = RefCell::new(Vec::new());
    let cancel = || {
        checks.set(checks.get() + 1);
        if checks.get() > 2 {
            Err(Error::cancelled("stop"))
        } else {
            Ok(())
        }
    };
    let mut encode = sized(|quality| usize::from(quality) * 10, &tried);
    let error = search(300, 90, &mut *encode, &mut |_| {}, &cancel).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(tried.borrow().len(), 2);

    let mut failing = |_: u8, _: &mut dyn FnMut(f64)| Err(Error::render("the encoder failed"));
    let error = search(300, 90, &mut failing, &mut |_| {}, &|| Ok(())).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Render);
}

/// Progress names each attempt as it starts, in order, with its quality; the fraction never
/// decreases, folds each encode's own progress in, and ends at 1.
#[test]
fn progress_reports_each_attempt_and_ends_at_one() {
    let (result, tried, reports) = run(2_500, 95, |quality| usize::from(quality) * 50);
    let found = result.unwrap();
    assert_eq!(found.quality, 50);
    let started: Vec<(u32, u8)> = reports
        .iter()
        .map(|report| (report.attempt, report.quality))
        .fold(Vec::new(), |mut started, at| {
            if started.last() != Some(&at) {
                started.push(at);
            }
            started
        });
    let expected: Vec<(u32, u8)> = (1..).zip(tried.iter().copied()).collect();
    assert_eq!(started[..expected.len()], expected);
    for pair in reports.windows(2) {
        assert!(pair[1].fraction >= pair[0].fraction, "{pair:?}");
    }
    assert!(
        reports
            .iter()
            .any(|report| report.fraction > 0.0 && report.fraction < 1.0)
    );
    assert_eq!(reports.last().unwrap().fraction, 1.0);
}

/// A photo-sized gradient with texture, so a JPEG's size follows its quality.
fn frame(width: u32, height: u32) -> Raster {
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for y in 0..height {
        for x in 0..width {
            let texture = ((x * 7 + y * 13) % 23) as u8;
            rgba.extend_from_slice(&[
                (x * 255 / width) as u8 ^ texture,
                (y * 255 / height) as u8,
                texture.wrapping_mul(9),
                255,
            ]);
        }
    }
    Raster {
        width,
        height,
        rgba: rgba.into(),
        source_fingerprint: "test".into(),
        snapshot_id: SnapshotId::default(),
    }
}

/// The file `encode_jpeg` writes for `frame` at `options`.
fn jpeg(frame: &Raster, options: &JpegOptions) -> Vec<u8> {
    let mut out = Vec::new();
    encode_jpeg(
        &mut out,
        frame,
        options,
        None,
        None,
        &mut |_| {},
        &|| Ok(()),
    )
    .unwrap();
    out
}

/// A held frame encoded under a limit set to one quality's file size comes back as that quality's
/// file, baseline and optimised; a limit no quality from 10 meets is refused; a cancel inside an
/// attempt ends the search.
#[test]
fn a_jpeg_under_a_limit_is_the_highest_fitting_file() {
    let frame = frame(320, 200);
    for optimise in [false, true] {
        let options = JpegOptions {
            quality: 90,
            chroma: Chroma::Half,
            optimise,
        };
        let at_60 = jpeg(
            &frame,
            &JpegOptions {
                quality: 60,
                ..options
            },
        );
        let at_61 = jpeg(
            &frame,
            &JpegOptions {
                quality: 61,
                ..options
            },
        );
        assert!(at_61.len() > at_60.len());
        let mut reports = Vec::new();
        let found = encode_jpeg_limited(
            (frame.width, frame.height),
            &frame.rgba,
            &options,
            at_60.len() as u64,
            None,
            None,
            &mut |report| reports.push(report),
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(found.quality, 60, "optimise {optimise}");
        assert!(found.bytes == at_60, "the file is quality 60's own");
        assert!(found.attempts <= MAX_ATTEMPTS);
        assert_eq!(reports.last().unwrap().fraction, 1.0);

        let error = encode_jpeg_limited(
            (frame.width, frame.height),
            &frame.rgba,
            &options,
            600,
            None,
            None,
            &mut |_| {},
            &|| Ok(()),
        )
        .unwrap_err();
        assert_eq!(error.data.as_deref().unwrap()["code"], LIMIT_UNREACHABLE);

        let checks = Cell::new(0);
        let error = encode_jpeg_limited(
            (frame.width, frame.height),
            &frame.rgba,
            &options,
            at_60.len() as u64,
            None,
            None,
            &mut |_| {},
            &|| {
                checks.set(checks.get() + 1);
                if checks.get() > 5 {
                    Err(Error::cancelled("stop"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
    }
}
