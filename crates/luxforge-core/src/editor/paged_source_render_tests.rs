//! A paged derived photograph on the GPU (`docs/design/panorama-merge.md`, "Rendering, samples and
//! export"): below its resident level's scale every view is planned over the overview, the stack
//! over the resident level, and its histogram counts the overview; at 100% and above the view is
//! planned over a window of level 0; samples and exports read level 0 through bounded windows, an
//! export's bands capped in bytes; and nothing plans or allocates level 0 whole.
use super::derived_source_editing_tests::scene;
use super::derived_source_tests::{catalog, directory};
use super::source::Preparing;
use super::test_support::mutation;
use super::*;
use crate::artifacts::derived_source::{CHUNK_SIDE, payload_work};
use crate::modules::Region;
use crate::render::gpu::{
    GpuStaging, GpuView, PreparedStream, STREAM_BAND_BYTES, STREAM_TILE_SIDES, band_sides,
    plan_read, plan_rest, plan_stream_sweeps,
};
use crate::tiles::ReadStage;
use crate::{PreviewSource, ProxyBounds, ProxyIdentity};
use luxforge_test_macros::slow;
use serde_json::{Value, json};
use std::fs;

/// Wider than the camera limits' 16,384, so the photograph is paged; its resident level is level
/// 1, 8,250 x 150.
const WIDTH: u32 = 16_500;
const HEIGHT: u32 = 300;
const LEVEL: (u32, u32) = (8_250, 150);

fn apply(service: &mut EditorService, asset: &AssetId, action: &str, parameters: Value) {
    let revision = service.state(asset).unwrap().revision;
    service
        .run_action(
            asset,
            mutation(revision, &format!("{action}-{revision}")),
            action,
            parameters,
        )
        .unwrap();
}

/// A catalog holding a paged photograph of [`scene`], prepared cold: its manifest and overview.
fn published(name: &str) -> (std::path::PathBuf, EditorService, AssetId) {
    let directory = directory(name);
    let (_, mut service, _) = catalog(&directory);
    let asset = service
        .publish_planes("merge.panorama", WIDTH, HEIGHT, &scene(WIDTH, HEIGHT))
        .unwrap();
    let needs = service.entry_needs(&asset, None).unwrap();
    service.prepare(&needs).unwrap();
    payload_work::take();
    (directory, service, asset)
}

/// A preview of the photograph before the source worker has read its resident level asks for
/// exactly that preparation, which reads the level's chunks and none of level 0; then Fit is
/// planned over the overview, the resident level reduced to the view, and 100% at the far corner
/// over a window of level 0 around the visible region, while the histogram's tiles count the
/// overview at both. Planning reads no pixel. The reference renderer's frame of the job is the
/// overview too, reduced to the view and counted at the level's size, and reads none of level 0.
#[test]
#[slow]
fn paged_source_render_views_are_planned_over_the_overview_or_a_level_0_window() {
    let (directory, mut service, asset) = published("paged-render-views");
    apply(
        &mut service,
        &asset,
        "set-basic",
        json!({"exposure": -0.5, "contrast": 20.0}),
    );
    apply(
        &mut service,
        &asset,
        "set-detail",
        json!({"sharpening": 40.0}),
    );
    let bounds = ProxyBounds {
        width: 1600,
        height: 1000,
    };
    let refused = service
        .preview_job(&asset, None, None, None, Some(bounds))
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::PreparationRequired, "{refused}");
    assert!(refused.detail.contains("resident level"), "{refused}");
    let needs = service.entry_needs(&asset, None).unwrap();
    let Preparing::Work(work, _) = service.preparation(&needs).unwrap() else {
        panic!("the resident level is work");
    };
    assert!(
        !work.decodes(),
        "the source holds its level, counted by the gate"
    );
    service.prepare(&needs).unwrap();
    let chunks = LEVEL.0.div_ceil(CHUNK_SIDE) * LEVEL.1.div_ceil(CHUNK_SIDE);
    assert_eq!(
        payload_work::take(),
        u64::from(chunks),
        "the resident level's chunks, none of level 0"
    );

    let job = service
        .preview_job(&asset, None, None, None, Some(bounds))
        .unwrap();
    let PreviewSource::Paged(paged) = job.evaluation.source() else {
        panic!("a paged photograph previews a paged source");
    };
    assert_eq!(paged.dimensions(), (WIDTH, HEIGHT));
    let level = paged.level().expect("the resident level is read");
    assert_eq!((level.width(), level.height()), LEVEL);
    let level_identity = paged.level_identity().unwrap();
    let l0 = job.evaluation.source().identity();
    assert_ne!(level_identity, l0, "level 0 is never taken for the level");
    let overview = job.evaluation.overview().unwrap().expect("an overview");
    assert!(overview.is_overview());
    assert_eq!(overview.full_stage(), (WIDTH, HEIGHT));
    assert_eq!(overview.source().dimensions(), LEVEL);

    // Fit: the resident level reduced to the view, the histogram's tiles over the overview.
    let fit = plan_rest(&job.evaluation, GpuView::Fit(bounds)).unwrap();
    let boundary = fit
        .view
        .boundary
        .as_ref()
        .expect("a boundary from the source");
    assert_eq!(boundary.key.source(), &level_identity);
    let plan = boundary.key.plan().expect("reduced to the view");
    assert!(plan.width <= bounds.width && plan.height <= bounds.height);
    let tiles = fit.tiles.unwrap().expect("tiles");
    assert_eq!(
        tiles.source, level_identity,
        "the histogram counts the overview"
    );
    assert_eq!((tiles.output.width, tiles.output.height), LEVEL);

    // 100% at the far corner: a window of level 0 around the region, bounded by its halo.
    let rect = Region {
        x0: WIDTH - 600,
        y0: HEIGHT - 200,
        width: 600,
        height: 200,
    };
    let region = plan_rest(
        &job.evaluation,
        GpuView::Region {
            rect,
            magnification: 1.0,
        },
    )
    .unwrap();
    let boundary = region
        .view
        .boundary
        .as_ref()
        .expect("a boundary from the source");
    assert_eq!(boundary.key.source(), &l0);
    assert!(boundary.key.plan().is_none(), "level 0 at full scale");
    let window = boundary.window.expect("a window of level 0");
    assert!(
        window.x0 <= rect.x0 && window.x1() >= rect.x1() && window.y1() >= rect.y1(),
        "{window:?} holds {rect:?}"
    );
    assert!(
        window.width <= rect.width + 2 * 64 && window.height <= HEIGHT,
        "{window:?} is the region and its halo, never level 0 whole"
    );
    let tiles = region.tiles.unwrap().expect("tiles");
    assert_eq!(
        tiles.source, level_identity,
        "the backdrop and counts are the overview's"
    );
    assert_eq!(payload_work::take(), 0, "planning reads no pixel");

    // The reference renderer's frame: the overview, shown reduced, its report the overview's.
    let mut job = job;
    job.analyse = true;
    job.proxy = Some(bounds);
    let mut queue = crate::PreviewQueue::default();
    let generation = queue.request(job);
    let result = luxforge_testbase::wait_for("the overview frame", || queue.poll());
    assert_eq!(result.generation, generation);
    let exact = result.exact().expect("the exact phase");
    assert!(exact.overview, "the frame is the overview");
    let raster = exact.result.as_ref().expect("the overview renders");
    assert_eq!(
        (raster.width, raster.height),
        LEVEL,
        "the stack over the level"
    );
    let display = exact.display.as_ref().expect("always shown reduced");
    assert!(display.width <= bounds.width && display.height <= bounds.height);
    let report = exact.report.as_ref().expect("a report");
    assert_eq!(
        (report.width, report.height),
        LEVEL,
        "the histogram counts the overview"
    );
    assert_eq!(
        (result.identity.width, result.identity.height),
        (WIDTH, HEIGHT),
        "framed on level 0's stage"
    );
    luxforge_testbase::wait_until("the job to end", || !queue.is_busy());
    assert_eq!(
        payload_work::take(),
        0,
        "the overview reads none of level 0"
    );
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}

/// A sample at the far corner plans one tile over a window of level 0 around the pixel; an
/// export's stream tiles level 0 at a side whose band of codes fits [`STREAM_BAND_BYTES`], each
/// tile over its own window; a 65,000-pixel-wide output takes no 2,048-row band; and a stack
/// whose staged sweeps would hold level 0 whole in stage textures is drawn chained, each tile's
/// chain over its own window. Nothing reads a pixel, and the whole-frame reference still refuses.
#[test]
#[slow]
fn paged_source_render_samples_and_exports_read_level_0_through_bounded_windows() {
    let (directory, mut service, asset) = published("paged-render-reads");
    apply(
        &mut service,
        &asset,
        "set-presence",
        json!({"clarity": 60.0, "texture": 30.0}),
    );
    let exact = service
        .exact_evaluation(&asset, AnalysisSelection::Current)
        .unwrap();
    let PreviewSource::Paged(paged) = exact.source() else {
        panic!("a paged source");
    };
    let l0: ProxyIdentity = exact.source().identity();
    assert!(
        paged.level().is_none(),
        "an exact read needs no resident level"
    );

    let pixel = Region {
        x0: WIDTH - 3,
        y0: HEIGHT - 2,
        width: 1,
        height: 1,
    };
    let read = plan_read(&exact, ReadStage::Output, pixel).unwrap();
    assert_eq!(read.source, l0);
    let window = read.tile.window;
    assert!(
        window.x0 <= pixel.x0 && window.x1() > pixel.x0,
        "{window:?}"
    );
    assert!(
        window.pixels() < u64::from(WIDTH) * u64::from(HEIGHT) / 8,
        "{window:?} is the pixel and its reach, never level 0 whole"
    );

    let prepared = PreparedStream::of(&exact).unwrap();
    assert_eq!(
        (prepared.output().width, prepared.output().height),
        (WIDTH, HEIGHT)
    );
    let sides = prepared.band_sides(&STREAM_TILE_SIDES);
    assert_eq!(
        sides,
        STREAM_TILE_SIDES.to_vec(),
        "16,500 x 2,048 codes fit"
    );
    for side in &sides {
        assert!(u64::from(WIDTH) * u64::from(*side) * 4 <= STREAM_BAND_BYTES);
    }
    let stream = prepared.stream(sides[0]).unwrap();
    assert_eq!(stream.source, l0);
    for tile in &stream.tiles {
        assert!(
            tile.window.width <= tile.rect.width + 2 * 512,
            "{:?} reads its own window",
            tile.window
        );
    }
    assert_eq!(
        band_sides(65_000, &STREAM_TILE_SIDES),
        vec![1024, 512],
        "a 65,000-pixel-wide band is never 2,048 rows high"
    );
    assert_eq!(
        band_sides(65_000, &[2048]),
        vec![2048],
        "the shortest when none fits"
    );

    match plan_stream_sweeps(&exact, 2 << 30).unwrap() {
        GpuStaging::Chained(_) => {}
        GpuStaging::Staged(sweeps) => panic!(
            "a {}x{} stage held whole in stage textures",
            sweeps.stage.width, sweeps.stage.height
        ),
    }
    assert_eq!(payload_work::take(), 0, "planning reads no pixel");
    let refused = service.render_current(&asset).unwrap_err();
    assert_eq!(refused.kind, ErrorKind::ResourceLimit, "{refused}");
    assert_eq!(payload_work::take(), 0, "the refusal reads no pixel");
    drop(service);
    fs::remove_dir_all(directory).unwrap();
}
