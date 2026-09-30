//! The Select grid's decoded previews: the reads it plans (cells on screen before the margin, each
//! file once, a refusal not asked again under the revision, a queued file read again once the owner
//! wakes this client), a better stage replacing the held preview, the byte budget and its eviction
//! (never a cell on screen, bytes accounted exactly), the decode worker on a generated JPEG, and a
//! real owner's `preview.read` answered and decoded into a handle of the cell's size.
use super::*;
use luxforge_core::{
    EditorService,
    catalog_types::{FileRecord, FileSignature, HeaderState, PreviewOrigin, VolumeId},
    seed::IndexSeeder,
};
use luxforge_testbase::{paths::temp_dir, wait_until};
use std::{fs, path::Path};

const REVISION: u64 = 7;
/// A Select cell's photograph box at scale 2.
const SIDE: u32 = 240;

fn file(id: i64) -> FileId {
    FileId(id)
}

fn cells(ids: &[i64]) -> Vec<(FileId, PreviewState)> {
    ids.iter()
        .map(|id| (file(*id), PreviewState::Pending))
        .collect()
}

fn wanted(revision: u64, visible: &[i64], margin: &[i64]) -> Wanted {
    Wanted {
        revision,
        visible: cells(visible),
        margin: cells(margin),
        side: SIDE,
    }
}

/// A cache whose decodes are planned but not started: the test hands their pixels in.
fn paused(budget: usize) -> SelectPreviews {
    let mut previews = SelectPreviews::with_budget(budget);
    previews.paused = true;
    previews
}

/// A grid preview of `file` at `stage`, `width` × `height`.
fn info(id: i64, origin: PreviewOrigin, width: u32, height: u32) -> PreviewInfo {
    PreviewInfo {
        item: PreviewItem::File { file_id: file(id) },
        tier: PreviewTier::Grid,
        path: PathBuf::from(format!(
            "/c.index/previews/files/{id}-{}.jpg",
            origin.as_str()
        )),
        width,
        height,
        origin,
        approximate: false,
        bytes: 40_000,
        key: format!("file:{id}:sig:grid:{}", origin.as_str()),
    }
}

fn ready(file: FileId) -> Result<PreviewAnswer, Refusal> {
    Ok(PreviewAnswer::Ready {
        preview: info(file.0, PreviewOrigin::Embedded, 512, 341),
    })
}

fn queued(fallback: Option<PreviewInfo>) -> Result<PreviewAnswer, Refusal> {
    Ok(PreviewAnswer::Queued {
        job_id: luxforge_core::JobId::new(),
        fallback,
    })
}

fn refused(code: &str) -> Result<PreviewAnswer, Refusal> {
    Err(Refusal {
        code: code.into(),
        message: format!("{code} for the test"),
    })
}

/// The owner's answers to `batch`, file by file.
fn answer(batch: &ReadBatch, by: impl Fn(FileId) -> Result<PreviewAnswer, Refusal>) -> ReadAnswers {
    ReadAnswers {
        serial: batch.serial,
        revision: batch.revision,
        answers: batch
            .reads
            .iter()
            .map(|(file, _)| (*file, by(*file)))
            .collect(),
    }
}

/// `decode`'s pixels, as the worker would hand them over: one grey `width` × `height` frame.
fn decoded(decode: &Decode, width: u32, height: u32) -> Decoded {
    Decoded {
        decode: decode.clone(),
        result: Ok(DecodedPreview {
            width,
            height,
            rgba: vec![128; width as usize * height as usize * 4],
        }),
    }
}

/// The decode the last plan made for `id`.
fn planned(previews: &SelectPreviews, id: i64) -> Option<Decode> {
    previews
        .last_plan
        .iter()
        .find(|decode| decode.file == file(id))
        .cloned()
}

/// A file whose newest answer named a `width` × `height` preview, as if it had been read.
fn named(previews: &mut SelectPreviews, id: i64, width: u32, height: u32) {
    let entry = previews.entries.entry(file(id)).or_default();
    entry.read = Read::Ready;
    entry.source = Some(Source::from(info(
        id,
        PreviewOrigin::Embedded,
        width,
        height,
    )));
}

/// The bytes the cache charges are exactly its handles' RGBA bytes, and it counts every handle.
fn assert_accounted(previews: &SelectPreviews) {
    let held: Vec<&Held> = previews
        .entries
        .values()
        .filter_map(|entry| entry.held.as_ref())
        .collect();
    let bytes: usize = held
        .iter()
        .map(|held| match &held.handle {
            Handle::Rgba {
                width,
                height,
                pixels,
                ..
            } => {
                assert_eq!(pixels.len(), *width as usize * *height as usize * 4);
                assert_eq!(held.bytes, pixels.len());
                pixels.len()
            }
            _ => panic!("a handle made from pixels"),
        })
        .sum();
    assert_eq!(previews.bytes, bytes, "bytes charged");
    assert_eq!(previews.handles, held.len(), "handles counted");
    assert!(previews.bytes <= previews.budget, "within the budget");
}

/// The files a batch reads, in order, with their priorities.
fn reads(batch: &ReadBatch) -> Vec<(i64, PreviewPriority)> {
    batch
        .reads
        .iter()
        .map(|(file, priority)| (file.0, *priority))
        .collect()
}

/// Cells on screen are read before the margin, the screen's last cell asked first so the lane,
/// which hands out the newest visible task first, reads its first first; a row with nothing to
/// draw is never asked; one batch is in flight at a time; a file is asked once; and each answer
/// names what the grid shows meanwhile.
#[test]
fn select_previews_read_cells_on_screen_before_the_margin_and_each_once() {
    use PreviewPriority::{Background, Visible};
    let mut previews = paused(DECODED_BUDGET_BYTES);
    let mut want = wanted(REVISION, &[1, 2, 3], &[4, 5]);
    want.visible.push((file(6), PreviewState::Unavailable));
    let batch = previews.plan_for(want.clone()).expect("a batch");
    assert_eq!(batch.revision, REVISION);
    assert_eq!(
        reads(&batch),
        [
            (3, Visible),
            (2, Visible),
            (1, Visible),
            (4, Background),
            (5, Background)
        ]
    );
    assert!(
        previews.plan_for(want.clone()).is_none(),
        "one batch in flight"
    );
    for id in 1..=5 {
        assert!(previews.loading(file(id)), "{id} waits for its preview");
    }
    assert!(
        !previews.loading(file(6)) && previews.unreadable(file(6)),
        "an unavailable row is not loading: it is Unreadable"
    );

    let thumbnail = info(2, PreviewOrigin::ExifThumbnail, 160, 107);
    let next = previews.answered(answer(&batch, |file| match file.0 {
        1 | 5 => ready(file),
        2 => queued(Some(thumbnail.clone())),
        3 => queued(None),
        _ => refused("unsupported-input"),
    }));
    assert!(next.is_none(), "every wanted file was asked once");
    assert!(previews.plan_for(want.clone()).is_none(), "and not again");
    assert!(previews.unreadable(file(4)));
    assert!(!previews.loading(file(4)));
    assert!(!previews.unreadable(file(3)) && previews.loading(file(3)));
    // Decodes: cells on screen first, each fitted to the cell and never past its preview: the
    // embedded previews at the cell's side, the thumbnail at its own.
    let plan: Vec<(i64, &str, u32)> = previews
        .last_plan
        .iter()
        .map(|decode| (decode.file.0, decode.key.as_str(), decode.side))
        .collect();
    assert_eq!(
        plan,
        [
            (1, "file:1:sig:grid:embedded", SIDE),
            (2, "file:2:sig:grid:exif-thumbnail", 160),
            (5, "file:5:sig:grid:embedded", SIDE)
        ]
    );

    // A newly shown cell is the only one asked.
    let mut more = want.clone();
    more.visible.push((file(7), PreviewState::Thumbnail));
    let batch = previews.plan_for(more).expect("the new cell");
    assert_eq!(reads(&batch), [(7, Visible)]);
    // At most one batch's worth at a time.
    previews.answered(answer(&batch, ready));
    let many: Vec<i64> = (100..100 + READ_BATCH as i64 + 10).collect();
    let batch = previews
        .plan_for(wanted(REVISION, &many, &[]))
        .expect("a batch");
    assert_eq!(batch.reads.len(), READ_BATCH);
}

/// A refusal is not asked again under the revision, even after a wake; a new revision asks every
/// wanted file once more; a queued file, and one the lane's full queue refused, is read again once
/// the owner wakes this client; and answers for an older revision are dropped and asked again.
#[test]
fn select_previews_ask_again_only_after_a_wake_or_a_new_revision() {
    let mut previews = paused(DECODED_BUDGET_BYTES);
    let want = wanted(REVISION, &[1, 2, 3], &[]);
    let batch = previews.plan_for(want.clone()).expect("a batch");
    previews.answered(answer(&batch, |file| match file.0 {
        1 => refused("source-unavailable"),
        2 => queued(None),
        _ => refused("resource-limit"),
    }));
    assert!(previews.plan_for(want.clone()).is_none());
    assert!(
        previews.woken().is_none(),
        "a signal the worker posted reads nothing"
    );
    // The owner wrote a preview this client waits on.
    previews.woken.store(true, Ordering::Release);
    let batch = previews.woken().expect("the queued files");
    let mut asked: Vec<i64> = batch.reads.iter().map(|(file, _)| file.0).collect();
    asked.sort_unstable();
    assert_eq!(
        asked,
        [2, 3],
        "the refused file is not asked again under the revision"
    );
    previews.answered(answer(&batch, ready));
    assert!(previews.plan_for(want.clone()).is_none());

    // A new revision asks every wanted file once more.
    let batch = previews
        .plan_for(wanted(REVISION + 1, &[1, 2, 3], &[]))
        .expect("the new revision's reads");
    assert_eq!(batch.reads.len(), 3);
    // It is overtaken by another before it answers: its answers are dropped and asked again.
    let newer = wanted(REVISION + 2, &[1, 2, 3], &[]);
    assert!(previews.plan_for(newer.clone()).is_none(), "one in flight");
    assert!(previews.answered(answer(&batch, ready)).is_some());
    assert!(
        previews
            .entries
            .values()
            .all(|entry| entry.read == Read::Asking),
        "asked again under the newest revision"
    );
}

/// A wake taken while a batch is in flight may be for a preview that batch then answers `queued`:
/// those files are read again at once rather than waiting for a wake that has already come.
#[test]
fn select_previews_a_wake_during_a_read_is_not_lost() {
    let mut previews = paused(DECODED_BUDGET_BYTES);
    let want = wanted(REVISION, &[1, 2], &[]);
    let batch = previews.plan_for(want).expect("a batch");
    previews.woken.store(true, Ordering::Release);
    assert!(previews.woken().is_none(), "one batch in flight");
    let again = previews
        .answered(answer(&batch, |_| queued(None)))
        .expect("read again at once");
    assert_eq!(again.reads.len(), 2);
    // Answered queued once more, with no wake since: they wait for the next one.
    assert!(
        previews
            .answered(answer(&again, |_| queued(None)))
            .is_none()
    );
    assert!(previews.woken().is_none());
}

/// A file the lane is still reading is answered with the same job and waits for the next wake; one
/// answered with another job after a wake had its read end without a preview (the file is gone or
/// its volume is not connected): it is not asked again under the revision, so a missing file is not
/// read, queued and woken for in a loop, and the screen settles on its placeholder.
#[test]
fn select_previews_a_read_that_ended_without_a_preview_is_not_asked_again() {
    let mut previews = paused(DECODED_BUDGET_BYTES);
    let want = wanted(REVISION, &[1], &[]);
    let job = luxforge_core::JobId::new();
    let same = |job: &luxforge_core::JobId| {
        let job = job.clone();
        move |_| {
            Ok(PreviewAnswer::Queued {
                job_id: job.clone(),
                fallback: None,
            })
        }
    };
    let batch = previews.plan_for(want.clone()).expect("a batch");
    previews.answered(answer(&batch, same(&job)));
    assert!(!previews.settled(), "the cell on screen is loading");
    // Woken for another file's preview while this one's read runs on: the same job answers.
    previews.woken.store(true, Ordering::Release);
    let batch = previews.woken().expect("read again after the wake");
    previews.answered(answer(&batch, same(&job)));
    assert_eq!(previews.entries[&file(1)].read, Read::Queued);
    // Its read ended without a preview: read again, the lane starts another job.
    previews.woken.store(true, Ordering::Release);
    let batch = previews.woken().expect("read again after the wake");
    previews.answered(answer(&batch, same(&luxforge_core::JobId::new())));
    assert!(matches!(
        &previews.entries[&file(1)].read,
        Read::Refused(refusal) if refusal.code == "not-read"
    ));
    assert!(previews.settled(), "nothing on screen is still loading");
    previews.woken.store(true, Ordering::Release);
    assert!(
        previews.woken().is_none(),
        "not asked again under the revision"
    );
    assert!(
        !previews.unreadable(file(1)),
        "a placeholder, not Unreadable"
    );
    assert!(
        previews.plan_for(wanted(REVISION + 1, &[1], &[])).is_some(),
        "a new revision asks again"
    );
}

/// The thumbnail stage is drawn while the lane reads the embedded preview; the embedded preview,
/// once decoded, replaces it in place — the cell is never blank in between — and a late decode of
/// the stage it replaced changes nothing. The handle keeps its id while it is held. An answer that
/// names nothing cached drops what is held.
#[test]
fn select_previews_a_better_stage_replaces_the_held_one() {
    let mut previews = paused(DECODED_BUDGET_BYTES);
    let want = wanted(REVISION, &[1], &[]);
    let batch = previews.plan_for(want.clone()).expect("a batch");
    let thumbnail = info(1, PreviewOrigin::ExifThumbnail, 160, 107);
    previews.answered(answer(&batch, |_| queued(Some(thumbnail.clone()))));
    let soft = planned(&previews, 1).expect("the thumbnail is decoded");
    previews.adopt(decoded(&soft, 160, 107));
    assert_eq!(previews.size(file(1)), Some((160, 107)));
    let id = previews.handle(file(1)).expect("drawn soft").id();
    assert_eq!(previews.handle(file(1)).map(Handle::id), Some(id), "one id");
    assert!(
        previews.plan_for(want.clone()).is_none(),
        "held at its own size: nothing more"
    );
    assert!(
        previews
            .last_plan
            .iter()
            .all(|decode| decode.key != soft.key)
    );

    // The lane wrote the embedded stage and woke this client.
    previews.woken.store(true, Ordering::Release);
    let batch = previews.woken().expect("read again");
    previews.answered(answer(&batch, ready));
    let sharp = planned(&previews, 1).expect("the embedded preview is decoded");
    assert_eq!(sharp.side, SIDE);
    assert_eq!(
        previews.handle(file(1)).map(Handle::id),
        Some(id),
        "the thumbnail stays until the embedded preview is decoded"
    );
    previews.adopt(decoded(&soft, 160, 107));
    assert_eq!(
        previews.handle(file(1)).map(Handle::id),
        Some(id),
        "a late decode of the older stage changes nothing"
    );
    previews.adopt(decoded(&sharp, 240, 160));
    assert_eq!(previews.size(file(1)), Some((240, 160)));
    assert_ne!(previews.handle(file(1)).map(Handle::id), Some(id));
    assert_eq!(previews.handles, 1);
    assert_eq!(previews.bytes, 240 * 160 * 4);
    assert_accounted(&previews);
    // The same decode again is not a second handle.
    let held = previews.handle(file(1)).map(Handle::id);
    previews.adopt(decoded(&sharp, 240, 160));
    assert_eq!(previews.handle(file(1)).map(Handle::id), held);

    // A larger cell decodes the preview again, up to its own size only.
    let mut larger = want.clone();
    larger.side = 2 * SIDE;
    assert!(previews.plan_for(larger.clone()).is_none());
    assert_eq!(planned(&previews, 1).map(|decode| decode.side), Some(480));
    let mut largest = larger;
    largest.side = MAX_SIDE;
    previews.plan_for(largest);
    assert_eq!(planned(&previews, 1).map(|decode| decode.side), Some(512));

    // The file changed, and nothing valid is cached for it any more.
    let batch = previews
        .plan_for(wanted(REVISION + 1, &[1], &[]))
        .expect("asked again");
    previews.answered(answer(&batch, |_| queued(None)));
    assert!(previews.handle(file(1)).is_none());
    assert!(previews.loading(file(1)));
    assert_eq!((previews.bytes, previews.handles), (0, 0));
}

/// Past the budget the least recently wanted previews go first; a cell on screen is never evicted,
/// and a margin cell only for a cell on screen; a preview that cannot fit so is dropped and not
/// decoded again until the wanted cells change. The bytes charged are exactly the handles' RGBA
/// bytes throughout.
#[test]
fn select_previews_evict_the_least_recently_wanted_never_a_cell_on_screen() {
    const EACH: usize = 100 * 100 * 4;
    let mut previews = paused(3 * EACH);
    let show = |previews: &mut SelectPreviews, visible: &[i64], margin: &[i64]| {
        previews.plan_for(wanted(REVISION, visible, margin));
        for id in visible.iter().chain(margin) {
            if previews.entries[&file(*id)].source.is_none() {
                named(previews, *id, 100, 100);
            }
        }
        previews.plan_for(wanted(REVISION, visible, margin));
        for decode in previews.last_plan.clone() {
            previews.adopt(decoded(&decode, 100, 100));
            assert_accounted(previews);
        }
        previews.plan_for(wanted(REVISION, visible, margin));
    };
    let held = |previews: &SelectPreviews| {
        let mut held: Vec<i64> = previews
            .entries
            .iter()
            .filter(|(_, entry)| entry.held.is_some())
            .map(|(file, _)| file.0)
            .collect();
        held.sort_unstable();
        held
    };
    show(&mut previews, &[1, 2], &[3]);
    assert_eq!(held(&previews), [1, 2, 3]);
    assert_eq!(previews.bytes, 3 * EACH);
    // Scrolled on: the three no longer wanted give way, the oldest first.
    show(&mut previews, &[4, 5], &[6]);
    assert_eq!(held(&previews), [4, 5, 6]);
    assert!(
        previews.entries.len() == 3,
        "entries nothing wants or holds are forgotten"
    );
    assert_eq!(previews.dropped, 0);

    // A fourth cell on screen when all three held are on screen: it cannot fit.
    show(&mut previews, &[4, 5, 6, 7], &[]);
    assert_eq!(held(&previews), [4, 5, 6]);
    assert_eq!(previews.dropped, 1);
    assert!(
        planned(&previews, 7).is_none(),
        "not decoded again for the same cells"
    );
    // A margin cell may not evict another margin cell, nor a cell on screen.
    show(&mut previews, &[4], &[5, 6, 8]);
    assert_eq!(held(&previews), [4, 5, 6]);
    assert_eq!(previews.dropped, 2);
    // A cell on screen evicts a margin cell.
    show(&mut previews, &[4, 8], &[5, 6]);
    assert_eq!(held(&previews).len(), 3);
    assert!(held(&previews).contains(&8) && held(&previews).contains(&4));
    assert_accounted(&previews);
}

/// The handle count is bounded too: previews too small to reach the byte budget still give way,
/// the least recently wanted first.
#[test]
fn select_previews_hold_at_most_the_handle_count() {
    let mut previews = paused(DECODED_BUDGET_BYTES);
    let first: Vec<i64> = (0..MAX_HANDLES as i64).collect();
    let want = Wanted {
        revision: REVISION,
        visible: cells(&first),
        margin: Vec::new(),
        side: 1,
    };
    previews.plan_for(want.clone());
    for id in &first {
        named(&mut previews, *id, 1, 1);
    }
    previews.plan_for(want.clone());
    while !previews.last_plan.is_empty() {
        for decode in previews.last_plan.clone() {
            previews.adopt(decoded(&decode, 1, 1));
        }
        previews.plan_for(want.clone());
    }
    assert_eq!(previews.handles, MAX_HANDLES);
    previews.plan_for(Wanted {
        revision: REVISION,
        visible: cells(&[-1]),
        margin: Vec::new(),
        side: 1,
    });
    named(&mut previews, -1, 1, 1);
    previews.plan_for(Wanted {
        revision: REVISION,
        visible: cells(&[-1]),
        margin: Vec::new(),
        side: 1,
    });
    let decode = planned(&previews, -1).expect("the new cell");
    previews.adopt(decoded(&decode, 1, 1));
    assert_eq!(previews.handles, MAX_HANDLES);
    assert!(previews.handle(file(-1)).is_some());
    assert!(previews.handle(file(0)).is_none(), "the oldest gave way");
    assert_accounted(&previews);
}

/// Leaving Select forgets every handle, answer and plan; a batch still in flight answers nothing.
#[test]
fn select_previews_release_forgets_everything() {
    let mut previews = paused(DECODED_BUDGET_BYTES);
    let want = wanted(REVISION, &[1, 2], &[]);
    let batch = previews.plan_for(want.clone()).expect("a batch");
    previews.release();
    assert!(previews.answered(answer(&batch, ready)).is_none());
    assert!(previews.entries.is_empty());
    assert_eq!((previews.bytes, previews.handles), (0, 0));
    // Showing Select again asks again.
    assert!(previews.plan_for(want).is_some());
}

/// Red, green, blue and yellow quadrants: top left, top right, bottom left, bottom right.
const QUADRANTS: [[u8; 3]; 4] = [[220, 35, 45], [35, 190, 65], [40, 70, 220], [235, 195, 30]];

/// A JPEG of the four quadrants at `path`, through the tests' independent encoder.
fn quadrants_jpeg(path: &Path, width: u32, height: u32) {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        let quadrant = usize::from(y >= height / 2) * 2 + usize::from(x >= width / 2);
        image::Rgb(QUADRANTS[quadrant])
    });
    image
        .save_with_format(path, image::ImageFormat::Jpeg)
        .unwrap();
}

/// The colour at the centre of each quadrant of the JPEG at `path`, through the tests' independent
/// decoder.
fn jpeg_quadrants(path: &Path) -> [[u8; 3]; 4] {
    let image = image::open(path).unwrap().to_rgb8();
    let (width, height) = image.dimensions();
    [(1, 1), (3, 1), (1, 3), (3, 3)].map(|(x, y)| image.get_pixel(width * x / 4, height * y / 4).0)
}

/// The colour at the centre of each quadrant of `handle`'s pixels, in [`QUADRANTS`]' order.
fn quadrant_colours(handle: &Handle) -> [[u8; 3]; 4] {
    let Handle::Rgba {
        width,
        height,
        pixels,
        ..
    } = handle
    else {
        panic!("a handle made from pixels");
    };
    [(1, 1), (3, 1), (1, 3), (3, 3)].map(|(x, y)| {
        let at = ((height * y / 4) * width + width * x / 4) as usize * 4;
        [pixels[at], pixels[at + 1], pixels[at + 2]]
    })
}

fn assert_quadrants(handle: &Handle, quadrants: [[u8; 3]; 4], tolerance: u8) {
    for (actual, expected) in quadrant_colours(handle).into_iter().zip(quadrants) {
        assert!(
            actual
                .iter()
                .zip(expected)
                .all(|(actual, expected)| actual.abs_diff(expected) <= tolerance),
            "{actual:?} against {expected:?}"
        );
    }
}

/// Take the worker's decodes until `done`.
fn settle(
    previews: &mut SelectPreviews,
    what: &str,
    mut done: impl FnMut(&SelectPreviews) -> bool,
) {
    wait_until(what, || {
        previews.woken();
        done(previews)
    });
}

/// The worker decodes a generated JPEG at the cell's size, with its colours, off the update loop;
/// a preview that fails to decode is not asked for again.
#[test]
fn select_previews_the_worker_decodes_a_generated_jpeg_at_the_cell_size() {
    let dir = temp_dir("select-previews-worker");
    let path = dir.join("grid.jpg");
    quadrants_jpeg(&path, 480, 320);
    let mut previews = SelectPreviews::default();
    let want = wanted(REVISION, &[1, 2], &[]);
    previews.plan_for(want.clone());
    for (id, path) in [(1, path.clone()), (2, dir.join("gone.jpg"))] {
        let entry = previews.entries.get_mut(&file(id)).expect("wanted");
        entry.read = Read::Ready;
        entry.source = Some(Source {
            key: format!("file:{id}:sig:grid:embedded"),
            path,
            long_edge: 480,
        });
    }
    previews.plan_for(want.clone());
    settle(&mut previews, "the decodes", |previews| {
        previews.size(file(1)).is_some() && previews.entries[&file(2)].failed.is_some()
    });
    assert_eq!(previews.size(file(1)), Some((240, 160)));
    assert_quadrants(previews.handle(file(1)).expect("decoded"), QUADRANTS, 8);
    assert_eq!(previews.bytes, 240 * 160 * 4);
    assert!(previews.handle(file(2)).is_none());
    assert!(previews.plan_for(want).is_none());
    assert!(
        previews.planned.is_empty(),
        "nothing left to decode: the failure is not asked for again"
    );
    drop(previews);
    let _ = fs::remove_dir_all(&dir);
}

/// Through a real owner: a JPEG the index lists is read with `preview.read`, queued, written by
/// the preview lane, which wakes this client, read again ready, and decoded into a handle of the
/// cell's size with the photograph's colours.
#[test]
fn select_previews_a_real_owner_answers_and_the_preview_is_decoded() {
    let root = temp_dir("select-previews-owner");
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
    let photo = files[0];
    let (owner, join) = OwnerHandle::start(&catalog).expect("an owner");
    let client = owner.register();

    let mut previews = SelectPreviews::default();
    previews.watch(&owner, client);
    let want = Wanted {
        revision: 1,
        visible: vec![(photo, PreviewState::Pending)],
        margin: Vec::new(),
        side: SIDE,
    };
    let batch = previews.plan_for(want).expect("the cell is read");
    let answers = read(&owner, client, batch);
    assert!(
        matches!(answers.answers[0].1, Ok(PreviewAnswer::Queued { .. })),
        "{:?}",
        answers.answers
    );
    previews.answered(answers);
    wait_until("the preview written, read again and decoded", || {
        if let Some(batch) = previews.woken() {
            let answers = read(&owner, client, batch);
            previews.answered(answers);
        }
        previews.size(photo).is_some()
    });
    // The fixture is 480 × 320: its grid tier is that size, and the cell's 240 px a half of it.
    assert_eq!(previews.size(photo), Some((240, 160)));
    let entry = &previews.entries[&photo];
    assert_eq!(entry.read, Read::Ready);
    let source = entry.source.as_ref().expect("the tier");
    assert!(source.key.ends_with(":grid:embedded"), "{}", source.key);
    assert_eq!(source.long_edge, 480);
    assert_quadrants(
        previews.handle(photo).expect("drawn"),
        jpeg_quadrants(&path),
        16,
    );
    let summary = previews.summary();
    assert_eq!(summary["handles"], 1);
    assert_eq!(summary["bytes"], 240 * 160 * 4);
    assert_eq!(summary["queued"], 0);

    drop(previews);
    owner.stop();
    let _ = join.join();
    let _ = fs::remove_dir_all(&root);
}
