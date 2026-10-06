//! The 100% region: exactness against the upright decode for every EXIF orientation, the frame
//! and its bounds, what a region decode allocates, the full-size rule, the development slot's one
//! development at a time, its cancellation and its kept frame, the developed tiers, the answer's
//! JPEG and, with the supplied RAW files, both paths on real cameras.
use super::*;
use crate::{EditorService, ErrorKind, open_source_bytes};
use luxforge_testbase::{Gate, paths, wait_until};
use sha2::{Digest, Sha256};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::atomic::{AtomicUsize, Ordering},
    thread,
};

fn fixture(name: &str) -> PathBuf {
    paths::fixture(&format!("s0/{name}"))
}

fn signature(path: &Path) -> FileSignature {
    FileSignature::of(&std::fs::metadata(path).unwrap())
}

fn rect(x: u32, y: u32, width: u32, height: u32) -> PixelRect {
    PixelRect {
        x,
        y,
        width,
        height,
    }
}

fn request(path: &Path, kind: SourceTag, rect: PixelRect) -> RegionRequest {
    RegionRequest {
        path: path.to_path_buf(),
        kind,
        signature: signature(path),
        rect,
        frame: None,
    }
}

/// `rect` cut from an upright RGBA8 frame `width` pixels wide, independently of the code under
/// test.
fn cut(rgba: &[u8], width: u32, rect: PixelRect) -> Vec<u8> {
    let mut out = Vec::new();
    for y in rect.y..rect.y + rect.height {
        let start = ((y * width + rect.x) * 4) as usize;
        out.extend_from_slice(&rgba[start..start + rect.width as usize * 4]);
    }
    out
}

/// Every rectangle of a JPEG original, at its corners and edges, of one pixel, unaligned with its
/// MCUs and of the whole frame, is the same rectangle cut from the file's full upright decode,
/// byte for byte, for all eight EXIF orientations, and says it is the file's own pixels. The file
/// is only read.
#[test]
fn an_embedded_region_is_the_same_rectangle_cut_from_the_upright_decode() {
    for orientation in 1..=8 {
        let path = fixture(&format!("orientation-{orientation}.jpg"));
        let bytes = std::fs::read(&path).unwrap();
        let upright = open_source_bytes(bytes.clone()).unwrap();
        let (w, h) = (upright.width, upright.height);
        let frame = FrameSize {
            width: w,
            height: h,
        };
        for r in [
            rect(0, 0, 1, 1),
            rect(w - 1, 0, 1, 1),
            rect(0, h - 1, 1, 1),
            rect(w - 1, h - 1, 1, 1),
            rect(0, 0, 40, 24),
            rect(w - 40, 0, 40, 24),
            rect(0, h - 24, 40, 24),
            rect(w - 40, h - 24, 40, 24),
            rect(13, 7, 37, 29),
            rect(w / 2 - 3, h / 2 - 5, 17, 9),
            rect(0, 5, w, 1),
            rect(5, 0, 1, h),
            rect(0, 0, w, h),
        ] {
            let image = region(&request(&path, SourceTag::Jpeg, r), &Cancel::new()).unwrap();
            assert_eq!(image.origin, PreviewOrigin::Embedded);
            assert_eq!((image.rect, image.frame), (r, frame));
            assert!(
                image.rgba == cut(&upright.rgba, w, r),
                "orientation {orientation}, {r:?}"
            );
            assert_eq!(
                embedded_region(&request(&path, SourceTag::Jpeg, r), &Cancel::new()).unwrap(),
                EmbeddedRegion::Region(image)
            );
        }
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

/// A rectangle running past the frame is clamped to it and says so; one starting outside it, one
/// of no pixels and one past the region limit are refused before the file is decoded. A rectangle
/// against a named frame of another size keeps its size, its centre mapped proportionally; the
/// source's own size takes it as it is. A file that changed or is gone is unavailable.
#[test]
fn a_rectangle_past_the_frame_is_clamped_and_one_outside_it_refused() {
    let path = fixture("orientation-6.jpg");
    let upright = open_source_bytes(std::fs::read(&path).unwrap()).unwrap();
    let (w, h) = (upright.width, upright.height);
    let frame = FrameSize {
        width: w,
        height: h,
    };
    let answer = |request: &RegionRequest| region(request, &Cancel::new());

    let clamped = answer(&request(
        &path,
        SourceTag::Jpeg,
        rect(w - 10, h - 5, 64, 64),
    ))
    .unwrap();
    assert_eq!(clamped.rect, rect(w - 10, h - 5, 10, 5));
    assert_eq!(clamped.rgba, cut(&upright.rgba, w, clamped.rect));

    for (r, kind) in [
        (rect(w, 0, 1, 1), ErrorKind::Validation),
        (rect(0, h, 1, 1), ErrorKind::Validation),
        (rect(0, 0, 0, 5), ErrorKind::Validation),
        (rect(0, 0, 5, 0), ErrorKind::Validation),
        (rect(0, 0, 4097, 2049), ErrorKind::ResourceLimit),
    ] {
        assert_eq!(
            answer(&request(&path, SourceTag::Jpeg, r))
                .unwrap_err()
                .kind,
            kind,
            "{r:?}"
        );
    }

    // Twice the size: the centre (88, 128) of a 16 px square at (80, 120) is (44, 64) here.
    let mut doubled = request(&path, SourceTag::Jpeg, rect(80, 120, 16, 16));
    doubled.frame = Some(FrameSize {
        width: 2 * w,
        height: 2 * h,
    });
    let mapped = answer(&doubled).unwrap();
    assert_eq!((mapped.rect, mapped.frame), (rect(36, 56, 16, 16), frame));
    assert_eq!(mapped.rgba, cut(&upright.rgba, w, mapped.rect));
    // Starting inside the named frame, the mapped rectangle starts inside the source: its centre,
    // (647 / 2, 967 / 2) here, lies past the source's corner, and the rectangle is clamped to it.
    doubled.rect = rect(2 * w - 1, 2 * h - 1, 16, 16);
    assert_eq!(answer(&doubled).unwrap().rect, rect(315, 475, 5, 5));
    let mut own = request(&path, SourceTag::Jpeg, rect(80, 120, 16, 16));
    own.frame = Some(frame);
    assert_eq!(answer(&own).unwrap().rect, rect(80, 120, 16, 16));

    let mut stale = request(&path, SourceTag::Jpeg, rect(0, 0, 8, 8));
    stale.signature.len += 1;
    assert_eq!(
        answer(&stale).unwrap_err().kind,
        ErrorKind::SourceUnavailable
    );
    let mut gone = request(&path, SourceTag::Jpeg, rect(0, 0, 8, 8));
    gone.path = paths::temp_path("gone.jpg");
    assert_eq!(
        answer(&gone).unwrap_err().kind,
        ErrorKind::SourceUnavailable
    );
    let cancelled = Cancel::new();
    cancelled.cancel();
    assert_eq!(
        region(
            &request(&path, SourceTag::Jpeg, rect(0, 0, 8, 8)),
            &cancelled
        )
        .unwrap_err()
        .kind,
        ErrorKind::Cancelled
    );
}

/// A region decode allocates the rectangle, exactly, and a turned one one strip of the stored
/// rectangle's rows beside it: never the frame. (The decoder's one window row is
/// `luxforge-jpeg`'s, bounded by its own tests.)
#[test]
fn a_region_decode_allocates_its_rectangle_not_the_frame() {
    let stored = FrameSize {
        width: 480,
        height: 320,
    };
    for orientation in 1..=8 {
        let path = fixture(&format!("orientation-{orientation}.jpg"));
        let r = rect(13, 7, 37, 29);
        let (image, allocated) = allocations::record(|| {
            region(&request(&path, SourceTag::Jpeg, r), &Cancel::new()).unwrap()
        });
        let rectangle = 37 * 29 * 4;
        assert_eq!(
            (image.rgba.len(), image.rgba.capacity()),
            (rectangle, rectangle)
        );
        let region = stored_region(stored, orientation, r);
        assert_eq!(
            (region.width * region.height) as usize * 4,
            rectangle,
            "the stored rectangle is the upright one turned"
        );
        let strip = REGION_STRIP_ROWS.min(region.height as usize) * region.width as usize * 4;
        let expected = if orientation == 1 {
            vec![rectangle]
        } else {
            vec![rectangle, strip]
        };
        assert_eq!(allocated, expected, "orientation {orientation}");
        let frame = (stored.width * stored.height * 4) as usize;
        assert!(allocated.iter().sum::<usize>() * 20 < frame);
    }
}

/// The inventory's rule: a preview is full size when both its edges are at least 95% of the
/// visible image's, long edge against long edge, so the Nikon Z 6's JPEG is and the Fujifilm
/// X100VI's and the DJI Air 2S's are not.
#[test]
fn the_full_size_rule_follows_the_inventory() {
    let size = |width, height| FrameSize { width, height };
    // Nikon Z 6: a 6048 × 4024 JPEG of a 6048 × 4024 visible image, stored either way round.
    assert!(is_full_size(size(6048, 4024), size(6048, 4024)));
    assert!(is_full_size(size(4024, 6048), size(6048, 4024)));
    // Canon EOS R3: 0.99 of the long edge.
    assert!(is_full_size(size(6000, 4000), size(6048, 4032)));
    // Exactly 95% of both edges is full size; a pixel less on either is not.
    assert!(is_full_size(size(95, 95), size(100, 100)));
    assert!(!is_full_size(size(94, 95), size(100, 100)));
    assert!(!is_full_size(size(95, 94), size(100, 100)));
    // Fujifilm X100VI (0.57), Sony a6400 (0.27) and DJI Air 2S (0.18).
    assert!(!is_full_size(size(4416, 2944), size(7728, 5152)));
    assert!(!is_full_size(size(1616, 1080), size(6000, 4000)));
    assert!(!is_full_size(size(960, 640), size(5472, 3648)));
    // A camera cropping to 16:9: the full width, but not the height.
    assert!(!is_full_size(size(6048, 3402), size(6048, 4024)));
    assert!(!is_full_size(size(10, 10), size(0, 0)));
}

/// The frame every fake development makes.
const SIZE: FrameSize = FrameSize {
    width: 64,
    height: 48,
};

/// A scratch file standing in for a RAW, and its signature.
fn raw_file(dir: &Path, name: &str, contents: &[u8]) -> (PathBuf, FileSignature) {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    let signature = signature(&path);
    (path, signature)
}

fn raw_request(path: &Path, signature: FileSignature, rect: PixelRect) -> RegionRequest {
    RegionRequest {
        path: path.to_path_buf(),
        kind: SourceTag::Raw,
        signature,
        rect,
        frame: None,
    }
}

/// A `size` frame whose every pixel says where it is and which development made it.
fn frame_of(tag: u8, size: FrameSize) -> Developed {
    let mut rgba = Vec::with_capacity((size.width * size.height * 4) as usize);
    for y in 0..size.height {
        for x in 0..size.width {
            rgba.extend_from_slice(&[x as u8, y as u8, tag, 255]);
        }
    }
    Developed {
        size,
        rgba: Arc::new(rgba),
    }
}

/// `rect` of the frame [`frame_of`] makes.
fn pixels_of(tag: u8, rect: PixelRect) -> Vec<u8> {
    cut(&frame_of(tag, SIZE).rgba, SIZE.width, rect)
}

/// A development that counts itself in `count`, passes `gate` and makes [`frame_of`] `tag`.
fn counted<'a>(
    tag: u8,
    count: &'a AtomicUsize,
    gate: &'a Gate,
) -> impl FnOnce(File, &Cancel) -> Result<Developed, Error> + 'a {
    move |_: File, _: &Cancel| -> Result<Developed, Error> {
        count.fetch_add(1, Ordering::SeqCst);
        gate.pass();
        Ok(frame_of(tag, SIZE))
    }
}

/// A development that must not run.
fn never(_: File, _: &Cancel) -> Result<Developed, Error> {
    panic!("this caller must not develop")
}

/// `r` of the scratch RAW at `path` through [`developed_region_in`], uncancelled.
fn raw_region(
    developments: &Developments,
    path: &Path,
    signature: FileSignature,
    r: PixelRect,
    develop: impl FnOnce(File, &Cancel) -> Result<Developed, Error>,
) -> Result<RegionImage, Error> {
    developed_region_in(
        developments,
        &raw_request(path, signature, r),
        &Cancel::new(),
        develop,
    )
}

/// Two callers wanting two frames at once develop one after the other: the second waits in the
/// slot, not in a development, until the first ends. Each region is labelled a development and cut
/// from its own frame.
#[test]
fn the_fallback_develops_one_raw_at_a_time() {
    let dir = paths::temp_dir("preview-region-one-at-a-time");
    let (a, a_signature) = raw_file(&dir, "a.nef", b"a");
    let (b, b_signature) = raw_file(&dir, "b.nef", b"bb");
    let developments = Developments::new();
    let gate = &Gate::new();
    let (count, running, most) = (
        &AtomicUsize::new(0),
        &AtomicUsize::new(0),
        &AtomicUsize::new(0),
    );
    let develop = |tag: u8| {
        move |_: File, _: &Cancel| -> Result<Developed, Error> {
            let now = running.fetch_add(1, Ordering::SeqCst) + 1;
            most.fetch_max(now, Ordering::SeqCst);
            count.fetch_add(1, Ordering::SeqCst);
            gate.pass();
            running.fetch_sub(1, Ordering::SeqCst);
            Ok(frame_of(tag, SIZE))
        }
    };
    gate.shut();
    let r = rect(1, 2, 3, 4);
    thread::scope(|scope| {
        let first = scope.spawn(|| {
            developed_region_in(
                &developments,
                &raw_request(&a, a_signature, r),
                &Cancel::new(),
                develop(1),
            )
        });
        gate.wait_reached(1, "the first development");
        let second = scope.spawn(|| {
            developed_region_in(
                &developments,
                &raw_request(&b, b_signature, r),
                &Cancel::new(),
                develop(2),
            )
        });
        wait_until("the second caller waits for the first development", || {
            developments.waiting() == 1
        });
        assert_eq!(gate.reached(), 1, "the second caller has not started one");
        gate.open();
        for (answer, tag) in [(first.join().unwrap(), 1), (second.join().unwrap(), 2)] {
            let image = answer.unwrap();
            assert_eq!(image.origin, PreviewOrigin::Developed);
            assert_eq!((image.rect, image.frame), (r, SIZE));
            assert_eq!(image.rgba, pixels_of(tag, r));
        }
    });
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(
        most.load(Ordering::SeqCst),
        1,
        "never two developments at once"
    );
}

/// A caller waiting for the running development returns `cancelled` once its token is set and the
/// slot is woken, without developing and without waiting for the development to end; one whose
/// token is already set develops nothing.
#[test]
fn a_cancelled_waiter_returns_cancelled_without_developing() {
    let dir = paths::temp_dir("preview-region-cancelled");
    let (a, a_signature) = raw_file(&dir, "a.raf", b"a");
    let (b, b_signature) = raw_file(&dir, "b.raf", b"bb");
    let developments = Developments::new();
    let gate = Gate::new();
    let count = AtomicUsize::new(0);
    gate.shut();
    let r = rect(0, 0, 8, 8);
    let cancel = Cancel::new();
    thread::scope(|scope| {
        let first = scope.spawn(|| {
            developed_region_in(
                &developments,
                &raw_request(&a, a_signature, r),
                &Cancel::new(),
                counted(1, &count, &gate),
            )
        });
        gate.wait_reached(1, "the first development");
        let waiter = scope.spawn(|| {
            developed_region_in(
                &developments,
                &raw_request(&b, b_signature, r),
                &cancel,
                never,
            )
        });
        wait_until("the second caller waits", || developments.waiting() == 1);
        cancel.cancel();
        developments.wake();
        assert_eq!(
            waiter.join().unwrap().unwrap_err().kind,
            ErrorKind::Cancelled
        );
        assert!(gate.holding(), "the first development is still running");
        gate.open();
        assert_eq!(first.join().unwrap().unwrap().rgba, pixels_of(1, r));
    });
    assert_eq!(
        developed_region_in(
            &developments,
            &raw_request(&b, b_signature, r),
            &cancel,
            never
        )
        .unwrap_err()
        .kind,
        ErrorKind::Cancelled
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

/// The slot keeps the last development: another region of the same frame, by either entry, is cut
/// from it without developing (and before the file is read as a RAW, which these are not); a new
/// frame replaces it, and the first frame then develops again.
#[test]
fn a_second_region_of_the_same_frame_reuses_its_development() {
    let dir = paths::temp_dir("preview-region-kept");
    let (a, a_signature) = raw_file(&dir, "a.dng", b"a");
    let (b, b_signature) = raw_file(&dir, "b.dng", b"bb");
    let developments = Developments::new();
    let gate = Gate::new();
    let count = AtomicUsize::new(0);
    let answer = |path: &Path, signature, r, tag| {
        raw_region(
            &developments,
            path,
            signature,
            r,
            counted(tag, &count, &gate),
        )
        .unwrap()
    };
    let first = answer(&a, a_signature, rect(0, 0, 8, 8), 1);
    assert_eq!(first.rgba, pixels_of(1, rect(0, 0, 8, 8)));
    // The pointer moves; the rectangle runs past the frame's corner and is clamped.
    let moved = raw_region(&developments, &a, a_signature, rect(60, 40, 8, 8), never).unwrap();
    assert_eq!(moved.rect, rect(60, 40, 4, 8));
    assert_eq!(moved.rgba, pixels_of(1, moved.rect));
    let kept = region_in(
        &developments,
        &raw_request(&a, a_signature, rect(5, 6, 7, 8)),
        &Cancel::new(),
        never,
    )
    .unwrap();
    assert_eq!(
        (kept.origin, kept.rgba),
        (PreviewOrigin::Developed, pixels_of(1, rect(5, 6, 7, 8)))
    );
    assert_eq!(count.load(Ordering::SeqCst), 1);

    assert_eq!(
        answer(&b, b_signature, rect(0, 0, 8, 8), 2).rgba,
        pixels_of(2, rect(0, 0, 8, 8))
    );
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert!(developments.kept(&a, &a_signature).is_none(), "released");
    answer(&a, a_signature, rect(0, 0, 8, 8), 1);
    assert_eq!(count.load(Ordering::SeqCst), 3);
    developments.release();
    assert!(developments.kept(&a, &a_signature).is_none());
}

/// A caller waiting while its own frame is developed takes that development when it ends.
#[test]
fn a_waiter_for_the_frame_being_developed_takes_it_without_developing() {
    let dir = paths::temp_dir("preview-region-same-frame");
    let (a, a_signature) = raw_file(&dir, "a.cr3", b"a");
    let developments = Developments::new();
    let gate = Gate::new();
    let count = AtomicUsize::new(0);
    gate.shut();
    thread::scope(|scope| {
        let first = scope.spawn(|| {
            developed_region_in(
                &developments,
                &raw_request(&a, a_signature, rect(0, 0, 4, 4)),
                &Cancel::new(),
                counted(1, &count, &gate),
            )
        });
        gate.wait_reached(1, "the development");
        let second = scope.spawn(|| {
            developed_region_in(
                &developments,
                &raw_request(&a, a_signature, rect(9, 9, 4, 4)),
                &Cancel::new(),
                never,
            )
        });
        wait_until("the second caller waits", || developments.waiting() == 1);
        gate.open();
        first.join().unwrap().unwrap();
        assert_eq!(
            second.join().unwrap().unwrap().rgba,
            pixels_of(1, rect(9, 9, 4, 4))
        );
    });
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

/// At most [`MAX_DEVELOPMENT_WAITERS`] callers wait; one more is refused at once.
#[test]
fn callers_past_the_waiting_bound_are_refused() {
    let dir = paths::temp_dir("preview-region-bound");
    let (a, a_signature) = raw_file(&dir, "a.nef", b"a");
    let (c, c_signature) = raw_file(&dir, "c.nef", b"ccc");
    let developments = Developments::new();
    let gate = Gate::new();
    let count = AtomicUsize::new(0);
    gate.shut();
    let r = rect(0, 0, 2, 2);
    thread::scope(|scope| {
        let first = scope.spawn(|| {
            developed_region_in(
                &developments,
                &raw_request(&a, a_signature, r),
                &Cancel::new(),
                counted(1, &count, &gate),
            )
        });
        gate.wait_reached(1, "the development");
        let waiters: Vec<_> = (0..MAX_DEVELOPMENT_WAITERS)
            .map(|_| {
                scope.spawn(|| {
                    developed_region_in(
                        &developments,
                        &raw_request(&a, a_signature, r),
                        &Cancel::new(),
                        never,
                    )
                })
            })
            .collect();
        wait_until("every waiter waits", || {
            developments.waiting() == MAX_DEVELOPMENT_WAITERS
        });
        assert_eq!(
            developed_region_in(
                &developments,
                &raw_request(&c, c_signature, r),
                &Cancel::new(),
                never,
            )
            .unwrap_err()
            .kind,
            ErrorKind::ResourceLimit
        );
        gate.open();
        first.join().unwrap().unwrap();
        for waiter in waiters {
            assert_eq!(waiter.join().unwrap().unwrap().rgba, pixels_of(1, r));
        }
    });
    assert_eq!(count.load(Ordering::SeqCst), 1);
}

/// A file changed since it was indexed is never answered from its old development; indexed again,
/// it develops again. A failed or panicking development keeps nothing and frees the slot, and a
/// JPEG original is never developed.
#[test]
fn a_changed_file_or_a_failed_development_is_not_kept() {
    let dir = paths::temp_dir("preview-region-changed");
    let (a, a_signature) = raw_file(&dir, "a.nef", b"a");
    let (b, b_signature) = raw_file(&dir, "b.nef", b"bb");
    let developments = Developments::new();
    let gate = Gate::new();
    let count = AtomicUsize::new(0);
    let r = rect(0, 0, 4, 4);
    raw_region(&developments, &a, a_signature, r, counted(1, &count, &gate)).unwrap();
    std::fs::write(&a, b"a longer file").unwrap();
    assert_eq!(
        raw_region(&developments, &a, a_signature, r, never)
            .unwrap_err()
            .kind,
        ErrorKind::SourceUnavailable
    );
    let changed = signature(&a);
    let again = raw_region(&developments, &a, changed, r, counted(2, &count, &gate)).unwrap();
    assert_eq!(again.rgba, pixels_of(2, r));
    assert_eq!(count.load(Ordering::SeqCst), 2);

    let refused = |_: File, _: &Cancel| -> Result<Developed, Error> {
        Err(Error::unsupported_input("a camera outside the catalog"))
    };
    assert_eq!(
        raw_region(&developments, &b, b_signature, r, refused)
            .unwrap_err()
            .kind,
        ErrorKind::UnsupportedInput
    );
    assert!(developments.kept(&a, &changed).is_none());
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        raw_region(
            &developments,
            &b,
            b_signature,
            r,
            |_: File, _: &Cancel| -> Result<Developed, Error> { panic!("a failing development") },
        )
    }));
    assert!(panicked.is_err());
    assert!(!developments.lock().running, "the slot is free again");
    let developed = raw_region(&developments, &b, b_signature, r, counted(3, &count, &gate));
    assert_eq!(developed.unwrap().rgba, pixels_of(3, r));

    let jpeg = fixture("orientation-1.jpg");
    assert_eq!(
        developed_region_in(
            &developments,
            &request(&jpeg, SourceTag::Jpeg, r),
            &Cancel::new(),
            never
        )
        .unwrap_err()
        .kind,
        ErrorKind::Validation
    );
}

/// A RAW without a usable preview gets its grid and loupe tiers from the same kept development
/// its 100% regions come from: downscaled to the long edge asked for, or the development itself
/// when it already fits.
#[test]
fn developed_previews_come_from_the_kept_development() {
    let dir = paths::temp_dir("preview-region-developed-preview");
    let (a, a_signature) = raw_file(&dir, "a.cr3", b"a");
    let developments = Developments::new();
    let count = &AtomicUsize::new(0);
    let size = FrameSize {
        width: 300,
        height: 200,
    };
    let grey = move |_: File, _: &Cancel| -> Result<Developed, Error> {
        count.fetch_add(1, Ordering::SeqCst);
        Ok(Developed {
            size,
            rgba: Arc::new([90, 140, 200, 255].repeat(300 * 200)),
        })
    };
    let preview = |max_side| {
        developed_preview_in(
            &developments,
            &a,
            &a_signature,
            max_side,
            &Cancel::new(),
            grey,
        )
    };
    let small = preview(150).unwrap();
    assert_eq!((small.width, small.height, small.frame), (150, 100, size));
    assert_eq!(small.rgba.len(), 150 * 100 * 4);
    assert!(
        small
            .rgba
            .chunks(4)
            .all(|pixel| pixel == [90, 140, 200, 255]),
        "an even frame stays even"
    );
    let whole = preview(300).unwrap();
    let kept = developments.kept(&a, &a_signature).unwrap();
    assert!(Arc::ptr_eq(&whole.rgba, &kept.rgba), "shared, not copied");
    let image = raw_region(&developments, &a, a_signature, rect(10, 10, 4, 4), never).unwrap();
    assert_eq!(image.origin, PreviewOrigin::Developed);
    assert_eq!(count.load(Ordering::SeqCst), 1);
    for side in [0, MAX_DEVELOPED_PREVIEW_SIDE + 1] {
        assert_eq!(preview(side).unwrap_err().kind, ErrorKind::Validation);
    }
}

/// A region's answer JPEG decodes to its rectangle, close to its pixels.
#[test]
fn an_encoded_region_decodes_to_its_pixels() {
    let path = fixture("orientation-1.jpg");
    let image = region(
        &request(&path, SourceTag::Jpeg, rect(200, 120, 64, 48)),
        &Cancel::new(),
    )
    .unwrap();
    let jpeg = encode_region(&image).unwrap();
    let mut decoder = luxforge_jpeg::Decoder::new(&jpeg, JPEG_LIMITS).unwrap();
    assert_eq!((decoder.width(), decoder.height()), (64, 48));
    let mut decoded = vec![0; 64 * 48 * 4];
    decoder.read_rows(&mut decoded).unwrap();
    let difference: u64 = decoded
        .iter()
        .zip(&image.rgba)
        .map(|(a, b)| u64::from(a.abs_diff(*b)))
        .sum();
    assert!(
        difference < decoded.len() as u64 * 2,
        "a mean difference of {} codes",
        difference as f64 / decoded.len() as f64
    );
}

fn sha256(path: &Path) -> String {
    format!("{:x}", Sha256::digest(std::fs::read(path).unwrap()))
}

/// The supplied RAW files through both paths: the Nikon Z 6's region from its full-size embedded
/// preview, equal to the same rectangle of the preview's whole upright decode and allocating only
/// the rectangle; the Fujifilm X100VI's and the DJI Air 2S's from a development, equal to the same
/// rectangle of the editor's own render of the imported Original, with the development's upright
/// size reported; the Air 2S's loupe tier from the kept development; every file unchanged. Set
/// `LUXFORGE_RAW_OWNER_DIR` to the directory holding them and run in release:
///
/// ```text
/// LUXFORGE_RAW_OWNER_DIR=/path/to/raw cargo test --release -p luxforge-core --lib \
///   preview_region -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires the supplied RAW files; run in release"]
fn supplied_raws_answer_from_their_embedded_preview_or_a_development() {
    let owner = PathBuf::from(std::env::var("LUXFORGE_RAW_OWNER_DIR").expect("RAW directory"));
    let cancel = Cancel::new();
    for (name, origin) in [
        ("nikon_z6.NEF", PreviewOrigin::Embedded),
        ("fujifilm_x100vi.RAF", PreviewOrigin::Developed),
        ("mavic_air_2s.DNG", PreviewOrigin::Developed),
    ] {
        let path = owner.join(name);
        let before = sha256(&path);
        let embedded = embedded_region(&request(&path, SourceTag::Raw, rect(0, 0, 1, 1)), &cancel)
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        let (frame, image) = match (&embedded, origin) {
            (EmbeddedRegion::Region(corner), PreviewOrigin::Embedded) => {
                let frame = corner.frame;
                let centre = rect(frame.width / 2 - 256, frame.height / 2 - 256, 512, 512);
                let (image, allocated) = allocations::record(|| {
                    region(&request(&path, SourceTag::Raw, centre), &cancel).unwrap()
                });
                assert_eq!(image.origin, PreviewOrigin::Embedded, "{name}");
                assert_eq!(image.rect, centre, "{name}");
                assert!(
                    allocated.iter().sum::<usize>() <= 2 * 512 * 512 * 4,
                    "{name}: {allocated:?}"
                );
                assert!(
                    image.rgba == embedded_reference(&path, centre),
                    "{name}: the region differs from the preview's upright decode"
                );
                (frame, image)
            }
            (EmbeddedRegion::NotFullSize { preview, visible }, PreviewOrigin::Developed) => {
                println!("{name}: preview {preview:?} is not full size for {visible:?}");
                let probe = region(&request(&path, SourceTag::Raw, rect(0, 0, 1, 1)), &cancel)
                    .unwrap_or_else(|error| panic!("{name}: {error:?}"));
                assert_eq!(probe.origin, PreviewOrigin::Developed, "{name}");
                let frame = probe.frame;
                let centre = rect(frame.width / 2 - 256, frame.height / 2 - 256, 512, 512);
                let kept = DEVELOPMENTS.kept(&path, &signature(&path)).unwrap();
                let image = region(&request(&path, SourceTag::Raw, centre), &cancel).unwrap();
                assert!(
                    Arc::ptr_eq(&kept, &DEVELOPMENTS.kept(&path, &signature(&path)).unwrap()),
                    "{name}: the second region developed again"
                );
                assert_eq!((image.origin, image.rect), (origin, centre), "{name}");
                let editor = editor_render(&path);
                assert_eq!(
                    (editor.width, editor.height),
                    (frame.width, frame.height),
                    "{name}: the development's size is the editor's"
                );
                assert!(
                    image.rgba == cut(&editor.rgba, editor.width, centre),
                    "{name}: the development differs from the editor's Original"
                );
                (frame, image)
            }
            (other, _) => panic!("{name}: expected {origin:?}, found {other:?}"),
        };
        if name == "mavic_air_2s.DNG" {
            let kept = DEVELOPMENTS.kept(&path, &signature(&path)).unwrap();
            let loupe =
                developed_preview(&path, &signature(&path), LOUPE_MAX_SIDE, &cancel).unwrap();
            assert_eq!(loupe.width.max(loupe.height), LOUPE_MAX_SIDE, "{name}");
            assert_eq!(loupe.frame, frame, "{name}");
            assert!(Arc::ptr_eq(
                &kept,
                &DEVELOPMENTS.kept(&path, &signature(&path)).unwrap()
            ));
        }
        let jpeg = encode_region(&image).unwrap();
        println!(
            "{name}: {:?} region {:?} of an upright {}×{} frame; answer JPEG {} bytes",
            image.origin,
            image.rect,
            frame.width,
            frame.height,
            jpeg.len()
        );
        assert_eq!(sha256(&path), before, "{name} changed");
    }
    release_development();
}

/// `rect` of the RAW's largest embedded JPEG, decoded whole and turned upright by the file's
/// orientation, independently of the region decode.
fn embedded_reference(path: &Path, rect: PixelRect) -> Vec<u8> {
    let flag = std::sync::atomic::AtomicBool::new(false);
    let mut previews = EmbeddedPreviews::open(
        File::open(path).unwrap(),
        luxforge_raw::MAX_EMBEDDED_READ_BUDGET,
        &flag,
    )
    .unwrap();
    let orientation = previews.listing().orientation;
    let index = previews.listing().largest_jpeg().unwrap().index;
    let EmbeddedImage::Jpeg(mut jpeg) = previews
        .extract(index, luxforge_raw::MAX_EMBEDDED_IMAGE_BYTES, &flag)
        .unwrap()
    else {
        panic!("a JPEG");
    };
    cut_after_last_eoi(&mut jpeg);
    let mut decoder = luxforge_jpeg::Decoder::new(&jpeg, JPEG_LIMITS).unwrap();
    let (width, height) = (decoder.width() as usize, decoder.height() as usize);
    let mut stored = vec![0; width * height * 4];
    decoder.read_rows(&mut stored).unwrap();
    let upright = FrameSize::upright(width as u32, height as u32, orientation);
    let mut turned = vec![0; stored.len()];
    for y in 0..height {
        for x in 0..width {
            let (ux, uy) = upright_position(orientation, width, height, x, y);
            let to = (uy * upright.width as usize + ux) * 4;
            let from = (y * width + x) * 4;
            turned[to..to + 4].copy_from_slice(&stored[from..from + 4]);
        }
    }
    cut(&turned, upright.width, rect)
}

/// The editor's own render of the RAW's Original, imported into a scratch catalog.
fn editor_render(path: &Path) -> crate::Raster {
    let mut service = EditorService::open(&paths::temp_catalog("preview-region-editor")).unwrap();
    let asset = service.import(path).unwrap().asset.id;
    service.render_current(&asset).unwrap()
}
