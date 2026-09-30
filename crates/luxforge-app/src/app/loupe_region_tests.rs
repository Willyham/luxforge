//! The focus check's regions: one out at a time with only the newest waiting (latest wins), an
//! unchanged pointer asking nothing, a wake kept until it can be used, stale answers dropped, a
//! failure said for its frame, and a real owner's `preview.region` cut from a JPEG's full-size
//! pixels and decoded into the inset's handle.
use super::*;
use luxforge_core::{
    EditorService,
    catalog_types::{FileId, FileRecord, FileSignature, HeaderState, PreviewOrigin, VolumeId},
    seed::IndexSeeder,
};
use luxforge_testbase::{paths::temp_dir, wait_until};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

fn item(id: i64) -> PreviewItem {
    PreviewItem::File {
        file_id: FileId(id),
    }
}

const FRAME: Dimensions = Dimensions {
    width: 6000,
    height: 4000,
};

/// A 616 × 418 rectangle of frame `id` at `x`.
fn request(id: i64, x: u32) -> RegionRequest {
    RegionRequest {
        item: item(id),
        rect: PixelRect {
            x,
            y: 100,
            width: 616,
            height: 418,
        },
        frame: FRAME,
    }
}

/// The region `request` asked for, cut as the lane cuts it, and its pixels.
fn ended(request: &RegionRequest, origin: PreviewOrigin) -> Result<Box<RegionRead>, String> {
    let answer = RegionAnswer {
        item: request.item.clone(),
        rect: request.rect,
        frame: request.frame,
        path: PathBuf::from("/c.index/previews/regions/1-1.jpg"),
        width: request.rect.width,
        height: request.rect.height,
        origin,
    };
    let decoded = DecodedPreview {
        width: request.rect.width,
        height: request.rect.height,
        rgba: vec![0; (request.rect.width * request.rect.height * 4) as usize],
    };
    Ok(Box::new(RegionRead::Ended(Ok((answer, decoded)))))
}

fn started(serial: u64, job: &str) -> RegionMessage {
    RegionMessage::Started {
        serial,
        result: Ok(job.into()),
    }
}

/// At most one region is out; of the rectangles asked for meanwhile only the newest waits, and it is
/// sent once the one out has landed. An unchanged pointer asks for nothing more.
#[test]
fn loupe_region_one_out_and_the_newest_waiting() {
    let mut focus = FocusCheck::default();
    assert_eq!(
        focus.want(Some(request(5, 10))),
        Some(Next::Start(1, request(5, 10)))
    );
    assert_eq!(focus.want(Some(request(5, 10))), None, "unchanged");
    assert_eq!(focus.want(Some(request(5, 20))), None, "one out");
    assert_eq!(focus.want(Some(request(5, 30))), None);
    assert!(focus.pending());
    assert_eq!(
        focus.update(started(1, "job-1")),
        None,
        "waits for the wake"
    );
    assert_eq!(focus.woken(true), Some(Next::Read(1, "job-1".into())));
    assert_eq!(
        focus.update(RegionMessage::Read {
            serial: 1,
            result: Ok(Box::new(RegionRead::Running)),
        }),
        None,
        "still being cut: the wake its end brings reads it again"
    );
    assert_eq!(focus.woken(false), None, "a decode's signal reads nothing");
    assert_eq!(focus.woken(true), Some(Next::Read(1, "job-1".into())));
    let next = focus.update(RegionMessage::Read {
        serial: 1,
        result: ended(&request(5, 10), PreviewOrigin::Embedded),
    });
    assert_eq!(
        next,
        Some(Next::Start(2, request(5, 30))),
        "the newest waited; the one between never went out"
    );
    let region = focus.region().expect("landed");
    assert_eq!(region.item, item(5));
    assert_eq!(region.rect, request(5, 10).rect);
    assert!(focus.handle(&region).is_some());
    assert_eq!(focus.frame_of(), Some((item(5), FRAME)));
    assert!(!focus.settled(&request(5, 30)), "the next region is out");
}

/// A wake that arrives before the region's job is known, or while its job is being read, is kept
/// and used as soon as it can be.
#[test]
fn loupe_region_a_wake_is_kept_until_it_can_be_used() {
    let mut focus = FocusCheck::default();
    focus.want(Some(request(5, 10)));
    assert_eq!(focus.woken(true), None, "no job yet");
    assert_eq!(
        focus.update(started(1, "job-1")),
        Some(Next::Read(1, "job-1".into())),
        "the region ended before its job's number arrived"
    );
    assert_eq!(focus.woken(true), None, "a read is out");
    assert_eq!(
        focus.update(RegionMessage::Read {
            serial: 1,
            result: Ok(Box::new(RegionRead::Running)),
        }),
        Some(Next::Read(1, "job-1".into())),
        "the wake during the read reads again"
    );
    assert_eq!(
        focus.update(RegionMessage::Read {
            serial: 1,
            result: ended(&request(5, 10), PreviewOrigin::Developed),
        }),
        None
    );
    assert!(focus.settled(&request(5, 10)));
    assert!(
        !focus.settled(&request(5, 20)),
        "not the rectangle asked for now"
    );
    assert_eq!(focus.region().unwrap().origin, PreviewOrigin::Developed);
}

/// Answers for a region no longer out are dropped; a failure is said for its own frame only and
/// the next region goes out; releasing forgets the region and the rectangle asked for.
#[test]
fn loupe_region_stale_answers_are_dropped_and_failures_said_for_their_frame() {
    let mut focus = FocusCheck::default();
    focus.want(Some(request(5, 10)));
    focus.want(Some(request(6, 10)));
    let next = focus.update(RegionMessage::Started {
        serial: 1,
        result: Err("source-unavailable: the file is gone".into()),
    });
    assert_eq!(next, Some(Next::Start(2, request(6, 10))));
    assert!(focus.error(&item(5)).is_some());
    assert!(focus.error(&item(6)).is_none());
    assert_eq!(focus.update(started(1, "job-old")), None, "a stale answer");
    focus.update(started(2, "job-2"));
    focus.release();
    assert!(!focus.pending() && focus.region().is_none());
    assert_eq!(
        focus.update(RegionMessage::Read {
            serial: 2,
            result: ended(&request(6, 10), PreviewOrigin::Embedded),
        }),
        None
    );
    assert!(
        focus.region().is_none(),
        "released: the late answer is dropped"
    );
    assert_eq!(
        focus.want(Some(request(6, 10))),
        Some(Next::Start(3, request(6, 10)))
    );
}

/// Through a real owner: `preview.region` of a JPEG the index lists, woken when it ends, read and
/// decoded whole into the inset's handle, cut from the file's own full-size pixels (`embedded`),
/// the rectangle as asked for in the frame named.
#[test]
fn loupe_region_a_real_owner_cuts_the_region_at_full_size() {
    let root = temp_dir("loupe-region-owner");
    let catalog = root.join("catalog.sqlite");
    let catalog_id = EditorService::open(&catalog)
        .expect("a catalog")
        .catalog_id()
        .to_owned();
    let photos = root.join("photos");
    fs::create_dir_all(&photos).unwrap();
    let path = photos.join("A.JPG");
    fs::copy(luxforge_testbase::paths::jpeg(), &path).unwrap();
    let mut seeder = IndexSeeder::create(&catalog, &catalog_id).expect("an index");
    let files = seeder
        .files(&[FileRecord {
            path: path.clone(),
            folder: photos.clone(),
            name: "A.JPG".into(),
            volume_id: VolumeId::parse("volume-0123456789").unwrap(),
            signature: FileSignature::of(&fs::metadata(&path).unwrap()),
            kind: luxforge_core::SourceTag::Jpeg,
            header: HeaderState::Ok(Box::default()),
            last_seen_ms: 0,
        }])
        .expect("the file");
    seeder.finish().expect("the index");
    let frame = PreviewItem::File { file_id: files[0] };
    let (owner, join) = OwnerHandle::start(&catalog).expect("an owner");
    let client = owner.register();
    let woke = Arc::new(AtomicBool::new(false));
    let flag = woke.clone();
    owner.watch_previews(
        client,
        Arc::new(move || flag.store(true, Ordering::Release)),
    );

    let mut focus = FocusCheck::default();
    let asked = RegionRequest {
        item: frame.clone(),
        rect: PixelRect {
            x: 100,
            y: 60,
            width: 200,
            height: 150,
        },
        frame: Dimensions {
            width: 480,
            height: 320,
        },
    };
    let mut next = focus.want(Some(asked.clone()));
    wait_until("the region cut, read and decoded", || {
        while let Some(step) = next.take() {
            next = match step {
                Next::Start(serial, request) => focus.update(RegionMessage::Started {
                    serial,
                    result: start_now(&owner, client, &request),
                }),
                Next::Read(serial, job) => focus.update(RegionMessage::Read {
                    serial,
                    result: read_now(&owner, client, &job).map(Box::new),
                }),
            };
        }
        next = focus.woken(woke.swap(false, Ordering::AcqRel));
        focus.settled(&asked)
    });
    assert_eq!(focus.error(&frame), None);
    let region = focus.region().expect("landed");
    assert_eq!(
        region.origin,
        PreviewOrigin::Embedded,
        "a JPEG is its own full-size image"
    );
    assert_eq!(region.rect, asked.rect);
    assert_eq!(region.frame, asked.frame);
    let Some(Handle::Rgba { width, height, .. }) = focus.handle(&region) else {
        panic!("a handle made from pixels");
    };
    assert_eq!((*width, *height), (200, 150), "at 100%, never resampled");
    owner.stop();
    let _ = join.join();
    let _ = fs::remove_dir_all(&root);
}
