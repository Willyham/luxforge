//! The loupe's decoded frames: the reads it plans (the frame on screen first, then the look-ahead,
//! each once, a queued frame read again once the owner wakes this client, and the reads of frames
//! passed cancelled), a stand-in drawn until its tier replaces it, a decode that lands for a frame
//! no longer shown never drawn as another's, the byte budget under a held arrow and its eviction
//! (never a frame on screen), and a real owner's `preview.read` of the loupe tier answered and
//! decoded into a handle of the screen's size.
use super::*;
use luxforge_core::{
    EditorService, JobId,
    catalog_types::{FileId, FileRecord, FileSignature, HeaderState, VolumeId},
    seed::IndexSeeder,
};
use luxforge_testbase::{paths::temp_dir, wait_until};
use std::fs;

/// The loupe's area at scale 2 on a 1440 × 900 window with the panels hidden.
const SCREEN: (u32, u32) = (2784, 1282);

fn file(id: i64) -> PreviewItem {
    PreviewItem::File {
        file_id: FileId(id),
    }
}

fn want(id: i64, shown: bool) -> Want {
    Want {
        item: file(id),
        pixels: SCREEN,
        shown,
    }
}

/// The active frame `active`, then the look-ahead `ahead`.
fn wanting(active: i64, ahead: &[i64]) -> Vec<Want> {
    std::iter::once(want(active, true))
        .chain(ahead.iter().map(|id| want(*id, false)))
        .collect()
}

/// A cache whose decodes are planned but not started: the test hands their pixels in.
fn paused(budget: usize) -> LoupeFrames {
    let mut frames = LoupeFrames::with_budget(budget);
    frames.paused = true;
    frames
}

/// A preview of frame `id` at `tier`, `width` × `height`.
fn info(id: i64, tier: PreviewTier, origin: PreviewOrigin, width: u32, height: u32) -> PreviewInfo {
    PreviewInfo {
        item: file(id),
        tier,
        path: PathBuf::from(format!(
            "/c.index/previews/files/{id}-{}.jpg",
            tier.as_str()
        )),
        width,
        height,
        origin,
        bytes: 400_000,
        key: format!("file:{id}:sig:{}:{}", tier.as_str(), origin.as_str()),
        approximate: false,
    }
}

fn ready(id: i64) -> Result<PreviewAnswer, Refusal> {
    Ok(PreviewAnswer::Ready {
        preview: info(id, PreviewTier::Loupe, PreviewOrigin::Embedded, 2560, 1707),
    })
}

fn queued(id: i64, job: &JobId) -> Result<PreviewAnswer, Refusal> {
    Ok(PreviewAnswer::Queued {
        job_id: job.clone(),
        fallback: Some(info(
            id,
            PreviewTier::Grid,
            PreviewOrigin::Embedded,
            512,
            341,
        )),
    })
}

fn answer(batch: &ReadBatch, by: impl Fn(i64) -> Result<PreviewAnswer, Refusal>) -> ReadAnswers {
    ReadAnswers {
        serial: batch.serial,
        answers: batch
            .reads
            .iter()
            .map(|item| match item {
                PreviewItem::File { file_id } => (item.clone(), by(file_id.0)),
                PreviewItem::Photo { .. } => unreachable!(),
            })
            .collect(),
    }
}

fn ids(batch: &ReadBatch) -> Vec<i64> {
    batch
        .reads
        .iter()
        .map(|item| match item {
            PreviewItem::File { file_id } => file_id.0,
            PreviewItem::Photo { .. } => unreachable!(),
        })
        .collect()
}

/// The pixels of `decode`, as the worker would hand them over.
fn decoded(decode: &Decode) -> Decoded {
    let (width, height) = (decode.side, decode.side * 2 / 3);
    Decoded {
        decode: decode.clone(),
        result: Ok(DecodedPreview {
            width,
            height,
            rgba: vec![0; (width * height * 4) as usize],
        }),
    }
}

fn planned(frames: &LoupeFrames, id: i64) -> Option<Decode> {
    frames
        .planned()
        .iter()
        .find(|decode| decode.item == file(id))
        .cloned()
}

/// Everything held is accounted: the bytes and handles the cache counts are the entries' own.
fn assert_accounted(frames: &LoupeFrames) {
    let held: Vec<&Held> = frames
        .entries
        .values()
        .filter_map(|entry| entry.held.as_ref())
        .collect();
    assert_eq!(
        frames.held_bytes(),
        (held.iter().map(|held| held.bytes).sum(), held.len())
    );
    assert!(frames.bytes <= frames.budget && frames.handles <= MAX_HANDLES);
}

/// The frame on screen is read first, then the look-ahead in its order, all at the look-ahead's
/// priority and each once; a frame the lane is making is read again only once the owner wakes
/// this client, and a wake during a read is not lost.
#[test]
fn loupe_frames_read_the_frame_on_screen_first_then_the_look_ahead() {
    let mut frames = paused(DECODED_BUDGET_BYTES);
    let batch = frames.want(wanting(5, &[6, 7, 8, 11])).expect("reads");
    assert_eq!(ids(&batch), vec![5, 6, 7, 8, 11]);
    assert!(batch.cancel.is_empty());
    assert!(
        frames.want(wanting(5, &[6, 7, 8, 11])).is_none(),
        "one batch at a time"
    );
    let job = JobId::new();
    let woken = frames.woken(true);
    assert!(woken.is_none(), "the batch is still in flight");
    let next = frames
        .answered(answer(&batch, |id| {
            if id == 5 { queued(id, &job) } else { ready(id) }
        }))
        .expect("woken during the read: frame 5 is read again at once");
    assert_eq!(ids(&next), vec![5]);
    let job_again = job.clone();
    assert!(
        frames
            .answered(answer(&next, move |id| queued(id, &job_again)))
            .is_none(),
        "queued again with the same job: waits for the owner's wake"
    );
    let again = frames.woken(true).expect("the owner wrote something");
    assert_eq!(ids(&again), vec![5]);
    assert!(frames.answered(answer(&again, ready)).is_none());
    assert!(frames.woken(true).is_none(), "nothing queued any more");
}

/// A frame waiting for its tier draws the stand-in the lane answered meanwhile, said to be one,
/// until the tier is decoded; the stand-in stays drawn while the tier decodes.
#[test]
fn loupe_frames_draw_the_stand_in_until_the_tier_lands() {
    let mut frames = paused(DECODED_BUDGET_BYTES);
    let batch = frames.want(wanting(5, &[])).unwrap();
    let job = JobId::new();
    frames.answered(answer(&batch, |id| queued(id, &job)));
    let stand_in = planned(&frames, 5).expect("the stand-in is decoded");
    assert_eq!(stand_in.side, 512, "at its own size, never enlarged");
    assert!(
        !frames.settled(&wanting(5, &[])),
        "the tier is still being read"
    );
    frames.adopt(decoded(&stand_in));
    let held = frames.held(&file(5));
    let picture = held.picture.expect("drawn");
    assert!(picture.stand_in && picture.key.contains(":grid:"));
    assert!(frames.handle(&picture).is_some());
    // The owner wrote the tier: read again, ready, decoded at the screen's size.
    let again = frames.woken(true).unwrap();
    frames.answered(answer(&again, ready));
    let tier = planned(&frames, 5).expect("the tier is decoded");
    assert_eq!(
        tier.side, 1923,
        "the 2560 × 1707 tier fitted to 1282 px high"
    );
    assert!(
        frames.held(&file(5)).picture.unwrap().stand_in,
        "the stand-in stays until the tier lands"
    );
    frames.adopt(decoded(&tier));
    let picture = frames.held(&file(5)).picture.unwrap();
    assert!(!picture.stand_in && picture.key.contains(":loupe:"));
    assert_eq!((picture.width, picture.height), (2560, 1707));
    assert!(frames.settled(&wanting(5, &[])));
    // The look-ahead's reads go on without unsettling the frame on screen.
    let ahead = frames.want(wanting(5, &[6])).expect("frame 6 is read");
    assert_eq!(ids(&ahead), vec![6]);
    assert!(
        frames.settled(&wanting(5, &[6])),
        "a look-ahead read is in flight"
    );
    assert_accounted(&frames);
}

/// Identity: a decode that lands for a frame no longer on screen is kept as that frame's own while
/// the look-ahead still wants it, and dropped once nothing does — never lent under the frame now
/// shown; a decode of a preview its frame's newest answer no longer names is dropped.
#[test]
fn loupe_frames_a_late_decode_is_never_drawn_as_another_frame() {
    let mut frames = paused(DECODED_BUDGET_BYTES);
    let batch = frames.want(wanting(5, &[6])).unwrap();
    frames.answered(answer(&batch, ready));
    let five = planned(&frames, 5).unwrap();
    let six = planned(&frames, 6).unwrap();
    // The person moves on to frame 9 before either decode lands; travelling back, frame 5 is
    // still ahead, frame 6 is wanted no more.
    if let Some(batch) = frames.want(wanting(9, &[5])) {
        frames.answered(answer(&batch, ready));
    }
    frames.adopt(decoded(&six));
    assert!(
        frames.held(&file(6)).picture.is_none(),
        "nothing wants it: dropped"
    );
    frames.adopt(decoded(&five));
    let nine = frames.held(&file(9));
    assert!(nine.picture.is_none(), "frame 9 has nothing of its own yet");
    let picture = frames
        .held(&file(5))
        .picture
        .expect("frame 5 keeps its own");
    assert_eq!(picture.item, file(5));
    assert!(
        frames
            .handle(&Picture {
                item: file(9),
                ..picture.clone()
            })
            .is_none(),
        "a handle is lent only for its own frame"
    );
    // A stale stage: frame 9's answer changes before its first decode lands.
    let stale = planned(&frames, 9).unwrap();
    let entry = frames.entries.get_mut(&file(9)).unwrap();
    entry.source.as_mut().unwrap().key = "file:9:changed:loupe:embedded".into();
    frames.adopt(decoded(&stale));
    assert!(frames.held(&file(9)).picture.is_none(), "dropped");
    assert_accounted(&frames);
}

/// A held arrow: stepping one frame at a time through 200 frames with the look-ahead ready ahead
/// of it, the plan handed to the worker never holds more decoded bytes than the budget, nor more
/// than its count; what is held never passes the budget; the frame on screen is always held once
/// decoded; and the reads of frames passed are cancelled with the next batch.
#[test]
fn loupe_frames_a_held_arrow_never_queues_more_decodes_than_the_budget_holds() {
    // About five screen-sized frames.
    let one = estimate(2560, 1707, side(2560, 1707, SCREEN));
    let budget = one * 5 + one / 2;
    let mut frames = paused(budget);
    let job = JobId::new();
    let mut cancelled = Vec::new();
    for active in 0..200i64 {
        let ahead: Vec<i64> = (active + 1..active + 4).chain([active + 10]).collect();
        let mut batch = frames.want(wanting(active, &ahead));
        while let Some(sent) = batch.take() {
            cancelled.extend(sent.cancel.iter().cloned());
            // The frame on screen and the one after are ready; the rest are still being read.
            batch = frames.answered(answer(&sent, |id| {
                if id <= active + 1 {
                    ready(id)
                } else {
                    queued(id, &job)
                }
            }));
        }
        assert!(
            frames.plan_bytes() <= budget,
            "step {active}: the plan holds {} of {budget} bytes",
            frames.plan_bytes()
        );
        assert!(frames.planned().len() <= MAX_PLAN);
        // The worker decodes the plan's head before the next key repeat.
        if let Some(head) = frames.planned().first().cloned() {
            frames.adopt(decoded(&head));
        }
        assert_accounted(&frames);
        let shown = frames.held(&file(active));
        assert!(
            shown
                .picture
                .is_some_and(|picture| picture.item == file(active)),
            "step {active}: the frame on screen is drawn, as its own"
        );
    }
    assert!(
        !cancelled.is_empty(),
        "the reads of frames passed were cancelled"
    );
    assert!(frames.dropped == 0, "nothing decoded had to be dropped");
}

/// Past the budget the least recently wanted go first; a frame on screen is never evicted, and a
/// look-ahead frame only for one on screen. A frame that cannot fit so is dropped, not decoded
/// again until the wanted frames change.
#[test]
fn loupe_frames_evict_the_least_recently_wanted_never_a_frame_on_screen() {
    let one = estimate(2560, 1707, side(2560, 1707, SCREEN));
    let mut frames = paused(one * 2);
    for active in [1, 2] {
        let batch = frames.want(wanting(active, &[])).unwrap();
        frames.answered(answer(&batch, ready));
        let decode = planned(&frames, active).unwrap();
        frames.adopt(decoded(&decode));
    }
    assert_eq!(frames.handles, 2);
    // Frame 3 on screen, 4 ahead: 3 evicts the least recently wanted (1).
    let batch = frames.want(wanting(3, &[4])).unwrap();
    frames.answered(answer(&batch, ready));
    let three = planned(&frames, 3).unwrap();
    frames.adopt(decoded(&three));
    assert!(frames.held(&file(1)).picture.is_none(), "the oldest went");
    assert!(frames.held(&file(3)).picture.is_some());
    // Frame 4, still wanted ahead, may evict frame 2 (no longer wanted), but never frame 3.
    let four = planned(&frames, 4).unwrap();
    frames.adopt(decoded(&four));
    assert!(frames.held(&file(3)).picture.is_some(), "on screen");
    assert!(frames.held(&file(4)).picture.is_some());
    assert!(frames.held(&file(2)).picture.is_none());
    assert_accounted(&frames);
    // A frame larger than what can be made room for is dropped and not planned again.
    let mut tiny = paused(one / 2);
    let batch = tiny.want(wanting(7, &[])).unwrap();
    tiny.answered(answer(&batch, ready));
    assert!(
        tiny.planned().is_empty(),
        "a decode larger than the budget is never planned"
    );
    assert!(tiny.held(&file(7)).picture.is_none());
}

/// A refused frame says why and is settled, having nothing to wait for; a read the lane ended
/// without the tier is not asked again. Releasing forgets everything and cancels what is queued.
#[test]
fn loupe_frames_a_refusal_is_settled_and_release_cancels_the_queued_reads() {
    let mut frames = paused(DECODED_BUDGET_BYTES);
    let batch = frames.want(wanting(5, &[6])).unwrap();
    let job = JobId::new();
    frames.answered(answer(&batch, |id| {
        if id == 5 {
            Err(Refusal {
                code: "unsupported-input".into(),
                message: "no usable preview".into(),
            })
        } else {
            queued(id, &job)
        }
    }));
    let held = frames.held(&file(5));
    assert_eq!(held.unavailable.as_deref(), Some("unsupported-input"));
    assert!(
        frames.settled(&wanting(5, &[6])),
        "nothing to wait for on screen"
    );
    assert!(
        !frames.settled(&wanting(9, &[])),
        "a frame not yet wanted is not"
    );
    // Frame 6's read ended without the tier: another job answers it now.
    let again = frames.woken(true).unwrap();
    assert_eq!(ids(&again), vec![6]);
    let other = JobId::new();
    frames.answered(answer(&again, |id| queued(id, &other)));
    assert!(frames.woken(true).is_none(), "not asked again");
    // A queued frame's job is cancelled when the loupe closes.
    let mut frames = paused(DECODED_BUDGET_BYTES);
    let batch = frames.want(wanting(5, &[6])).unwrap();
    frames.answered(answer(&batch, |id| queued(id, &job)));
    let released = frames.release().expect("the cancels");
    assert_eq!(released.cancel, vec![job.as_str().to_owned(); 2]);
    assert!(released.reads.is_empty());
    assert_eq!(frames.held_bytes(), (0, 0));
}

/// Through a real owner: a JPEG the index lists is read at the loupe's tier, queued with its grid
/// tier or nothing meanwhile, written by the preview lane, which wakes this client, read again
/// ready, and decoded on the worker into a handle of the screen's size.
#[test]
fn loupe_frames_a_real_owner_answers_and_the_tier_is_decoded() {
    let root = temp_dir("loupe-frames-owner");
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
    let item = PreviewItem::File { file_id: files[0] };
    let (owner, join) = OwnerHandle::start(&catalog).expect("an owner");
    let client = owner.register();
    let woke = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = woke.clone();
    owner.watch_previews(
        client,
        Arc::new(move || flag.store(true, Ordering::Release)),
    );

    let mut frames = LoupeFrames::default();
    let wants = vec![Want {
        item: item.clone(),
        pixels: (240, 240),
        shown: true,
    }];
    let batch = frames.want(wants.clone()).expect("the frame is read");
    frames.answered(read(&owner, client, batch));
    wait_until("the tier written, read again and decoded", || {
        if let Some(batch) = frames.woken(woke.swap(false, Ordering::AcqRel)) {
            frames.answered(read(&owner, client, batch));
        }
        frames.settled(&wants)
    });
    let picture = frames.held(&item).picture.expect("drawn");
    assert!(
        !picture.stand_in && picture.key.contains("loupe"),
        "{picture:?}"
    );
    // The fixture is 480 × 320: its loupe tier is that size, fitted to 240 px wide.
    assert_eq!((picture.width, picture.height), (480, 320));
    let Some(Handle::Rgba { width, height, .. }) = frames.handle(&picture) else {
        panic!("a handle made from pixels");
    };
    assert_eq!((*width, *height), (240, 160));
    assert_eq!(frames.summary()["handles"], 1);
    drop(frames);
    owner.stop();
    let _ = join.join();
    let _ = fs::remove_dir_all(&root);
}
