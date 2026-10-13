//! A paged derived photograph (a panorama merge's result wider than the camera limits,
//! `docs/design/panorama-merge.md`, "Rendering, samples and export") on this host's adapter: the
//! GPU tile worker reads its level 0 a window at a time through the source's chunk cache, so an
//! export streams it and a sample reads it bit for bit as the same planes held whole in memory
//! draw, its bands within their byte cap and a band wider than the runner's source split into
//! windows side by side; and the photo surface holds its resident level, with a window of level 0
//! beside it at 100% and above.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing: it is not GPU
//! evidence.
use super::{
    gpu_plan::install_output_encoding,
    gpu_tiles::GpuTiles,
    gpu_tiles_tests::{host_adapter, radial, recipe, registry},
    gpu_tiles_worker_tests::converted,
    tasks,
    testing::entry,
};
use crate::adapters;
use luxforge_core::{
    AssetId, BASIC_EFFECT, Cancel, DETAIL_EFFECT, Evaluation, Layer, OwnerHandle, PreviewRequest,
    PreviewSource, Recipe, Region, RenderContext, STREAM_BAND_BYTES, plan_read, qualification,
    tiles::{Answered, BandStream, ReadPixels, ReadStage, ReadValues, TileCall, TileService},
};
use luxforge_gpu::{GpuSource, headless::HeadlessSurface};
use luxforge_test_macros::slow;
use luxforge_testbase::{HANG, paths};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, mpsc},
};

/// Wider than the camera limits' 16,384, so the photograph is paged, and wider than the runner's
/// source of two 8,192-texel textures, so a band's windows are split.
const WIDTH: u32 = 16_500;
const HEIGHT: u32 = 300;

/// The parts of the stage the photo surface's drawing is held to, each within its 8,192-texel
/// boundary.
const PART: u32 = 5_500;

/// A catalog in a directory of its own holding a JPEG and a paged derived photograph of the
/// corpus's synthetic planes, published as a merge publishes its result; the planes.
fn published(name: &str) -> (PathBuf, PathBuf, AssetId, Vec<f32>) {
    let dir = paths::temp_dir(&format!("paged-{name}"));
    let anchor = dir.join("anchor.jpg");
    std::fs::copy(paths::jpeg(), &anchor).unwrap();
    let catalog = dir.join("catalog.sqlite");
    let planes = luxforge_testkit::derived::planes(WIDTH, HEIGHT);
    let asset = qualification::publish_derived(&catalog, &anchor, WIDTH, HEIGHT, &planes).unwrap();
    (dir, catalog, asset, planes)
}

/// `recipe` over `source`, bound as the catalog owner binds a saved entry's stack.
fn stack(source: &PreviewSource, recipe: &Recipe) -> Evaluation {
    Evaluation::new(
        Arc::new(registry()),
        RenderContext::new(),
        source.clone(),
        entry(&AssetId::new(), 1, None),
        recipe.clone(),
        None,
    )
}

/// Basic, Detail's sharpening, which reaches its neighbours, and a masked Basic.
fn edits() -> Recipe {
    recipe(
        vec![
            Layer::new(
                BASIC_EFFECT,
                json!({"exposure": -0.4, "contrast": 20.0, "vibrance": 25.0}),
            ),
            Layer::new(DETAIL_EFFECT, json!({"sharpening": 60.0})),
            Layer::new(BASIC_EFFECT, json!({"exposure": 0.6, "saturation": -20.0})),
        ],
        vec![(2, radial())],
    )
}

/// Every band of `stream` in order, stitched into its output stage's codes, each checked to start
/// where the last ended and to hold at most [`STREAM_BAND_BYTES`].
fn stitched(stream: BandStream) -> Vec<u8> {
    let (mut rgba, mut rows) = (Vec::new(), 0);
    for band in stream {
        let band = band.unwrap_or_else(|error| panic!("a band: {error:?}"));
        assert_eq!(band.y0, rows, "bands in order");
        assert!(
            band.rgba.len() as u64 <= STREAM_BAND_BYTES,
            "a band within its cap"
        );
        rows += band.rows;
        rgba.extend(band.rgba);
    }
    rgba
}

/// What one call on `service` over `stack` answers to reads of `rect`, as codes and as linear
/// values.
fn sampled(
    service: &dyn TileService,
    client: luxforge_core::ClientId,
    stack: &Evaluation,
    rect: Region,
) -> Vec<luxforge_core::tiles::ReadAnswer> {
    let (sender, answers) = mpsc::channel();
    let (done, finished) = mpsc::sync_channel(1);
    let held = stack.clone();
    service.submit(TileCall::caller(
        client,
        Cancel::new(),
        move |tiles, cancel| {
            let mut session = tiles.session(&held, cancel);
            for values in [ReadValues::Codes, ReadValues::Linear] {
                let _ = sender.send(session.read(ReadStage::Output, rect, values)?);
            }
            Ok(serde_json::Value::Null)
        },
        move |result| {
            let _ = done.send(result);
        },
    ));
    finished
        .recv_timeout(HANG)
        .expect("the call is answered")
        .unwrap_or_else(|error| panic!("the reads: {error:?}"));
    answers.try_iter().collect()
}

/// Through a catalog owner, the photograph's preview job holds its paged source with the resident
/// level read; a stack over it streams through the GPU tile worker in tiles of 2,048, each band's
/// tiles read in windows side by side, no band past its byte cap, and the stream is bit for bit
/// what the photo surface's own drawing draws over each part of the stage from the same planes
/// held whole; a sample at the far corner reads the stream's codes; and Auto tone's grid reads
/// the source's own values at its points.
#[test]
#[slow]
fn paged_source_render_exports_and_samples_level_0_through_windows_bit_for_bit() {
    let test = "paged_source_render_exports_and_samples_level_0_through_windows_bit_for_bit";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let (dir, catalog, asset, planes) = published("worker");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let job = tasks::ready_preview_job(&owner, PreviewRequest::new(client, asset.clone()))
        .unwrap_or_else(|error| panic!("{test}: the preview job: {error}"));
    let paged = job.evaluation.source().clone();
    let PreviewSource::Paged(preview) = &paged else {
        panic!("{test}: a paged photograph previews a paged source");
    };
    assert_eq!(preview.dimensions(), (WIDTH, HEIGHT));
    assert!(preview.level().is_some(), "the resident level is read");

    let service = GpuTiles::new(Some((backend.clone(), name.clone())), false);
    service.poison(true);
    let over_paged = stack(&paged, &edits());
    let before = service.figures();
    let bands = service
        .stream(&over_paged, &Cancel::new())
        .unwrap_or_else(|fallback| panic!("{test}: the paged stream: {fallback:?}"));
    assert_eq!(bands.answered(), &Answered::gpu());
    let streamed = stitched(bands);
    assert_eq!(streamed.len(), (WIDTH * HEIGHT * 4) as usize);
    let uploads = service.figures().uploads - before.uploads;
    assert!(
        uploads >= 2,
        "{test}: the band of {WIDTH} pixels is read in windows side by side ({uploads})"
    );

    // The photo surface's own drawing of each part of the stage over the planes held whole, the
    // window its plan reads handed alone.
    let window = adapters::open(&backend, &name)
        .unwrap_or_else(|unopened| panic!("{test}: the surface's device: {unopened:?}"));
    let mut surface = HeadlessSurface::new(&window.device, &window.queue);
    let planes_held = Arc::new(planes);
    let whole = GpuSource::planes(
        1,
        Arc::clone(&planes_held),
        (WIDTH, HEIGHT),
        [0, 0, WIDTH, HEIGHT],
        1,
    )
    .expect("the planes held whole");
    for (index, x0) in (0..WIDTH).step_by(PART as usize).enumerate() {
        let rect = Region {
            x0,
            y0: 0,
            width: PART.min(WIDTH - x0),
            height: HEIGHT,
        };
        let plan = plan_read(&over_paged, ReadStage::Output, rect)
            .unwrap_or_else(|fallback| panic!("{test}: {fallback:?}"));
        let read = plan.tile.window;
        let held = whole
            .window([read.x0, read.y0, read.width, read.height])
            .expect("the window inside the planes");
        let codes = surface
            .tile(&held, &converted(&plan, &held, 2 + index as u64))
            .unwrap_or_else(|fallback| panic!("{test}: the surface's drawing: {fallback:?}"));
        let drawn = plan.tile.rect;
        for (row, line) in codes.chunks_exact(drawn.width as usize).enumerate() {
            let at = ((drawn.y0 as usize + row) * WIDTH as usize + drawn.x0 as usize) * 4;
            let ours = &streamed[at..at + line.len() * 4];
            assert!(
                ours == line.concat().as_slice(),
                "{test}: row {} of the stream from x = {} is the surface's",
                drawn.y0 as usize + row,
                drawn.x0
            );
        }
    }

    let corner = Region {
        x0: WIDTH - 40,
        y0: HEIGHT - 30,
        width: 24,
        height: 16,
    };
    let answers = sampled(&service, client, &over_paged, corner);
    assert_eq!(answers.len(), 2);
    for answer in &answers {
        assert_eq!(answer.answered, Answered::gpu(), "{test}: the GPU reads it");
    }
    let ReadPixels::Codes(codes) = &answers[0].pixels else {
        panic!("{test}: codes");
    };
    for (index, code) in codes.iter().enumerate() {
        let (x, y) = (
            corner.x0 + index as u32 % corner.width,
            corner.y0 + index as u32 / corner.width,
        );
        let at = ((y * WIDTH + x) * 4) as usize;
        assert_eq!(
            code[..],
            streamed[at..at + 4],
            "{test}: the sample at ({x}, {y})"
        );
    }

    // Auto tone's grid of the stage Basic receives reads level 0 through windows on the GPU too:
    // each point is the source's own value at its nearest pixel, every grid row in the source.
    let (sender, grid) = mpsc::sync_channel(1);
    let held = over_paged.clone();
    service.submit(TileCall::caller(
        client,
        Cancel::new(),
        move |reads, cancel| {
            let read = luxforge_core::tiles::read_grid(&held, 0, reads, cancel)?;
            let _ = sender.send(read);
            Ok(serde_json::Value::Null)
        },
        |_| {},
    ));
    let read = grid
        .recv_timeout(HANG)
        .unwrap_or_else(|_| panic!("{test}: the Auto tone grid"));
    assert_eq!(
        read.answered,
        Answered::gpu(),
        "{test}: the GPU reads the grid"
    );
    let [columns, rows] = read.sample.grid;
    assert_eq!(read.sample.rgb.len(), (columns * rows) as usize);
    let plane = (WIDTH * HEIGHT) as usize;
    for (index, rgb) in read.sample.rgb.iter().enumerate() {
        let (column, row) = (index as u32 % columns, index as u32 / columns);
        let x = ((f64::from(column) + 0.5) * f64::from(WIDTH) / f64::from(columns)) as usize;
        let y = ((f64::from(row) + 0.5) * f64::from(HEIGHT) / f64::from(rows)) as usize;
        let at = y * WIDTH as usize + x;
        for (channel, value) in rgb.iter().enumerate() {
            let expected = planes_held[channel * plane + at];
            assert!(
                (value - expected).abs() <= 1e-6,
                "{test}: point ({x}, {y}) channel {channel}: {value} against {expected}"
            );
        }
    }
    eprintln!("{test}: the worker's figures {:?}", service.figures());
    owner.stop();
    join.join().unwrap();
    drop(dir);
}

/// The photo surface's window of level 0 at 100% and above ([`super::gpu_paged`]): a view's region
/// at the far corner asks for that region grown by the margin and clamped to level 0, read through
/// the source's chunk cache as the very planes published; a pan within it asks for no other read; a
/// pan beyond it cancels the read in flight, whose answer is then dropped; and a window is never
/// level 0 whole.
#[test]
#[slow]
fn paged_source_render_surface_windows_follow_the_view_and_drop_stale_reads() {
    use super::gpu_paged::{PagedWindows, WINDOW_MARGIN, WindowAnswer, read_window};
    let test = "paged_source_render_surface_windows_follow_the_view_and_drop_stale_reads";
    let (dir, catalog, asset, planes) = published("surface");
    let (owner, join) = OwnerHandle::start(&catalog).unwrap();
    let client = owner.register();
    let job = tasks::ready_preview_job(&owner, PreviewRequest::new(client, asset))
        .unwrap_or_else(|error| panic!("{test}: the preview job: {error}"));
    let PreviewSource::Paged(paged) = job.evaluation.source() else {
        panic!("{test}: a paged photograph previews a paged source");
    };
    let identity = paged.identity();
    let view = Region {
        x0: WIDTH - 900,
        y0: 40,
        width: 900,
        height: 200,
    };
    let mut windows = PagedWindows::default();
    assert!(windows.covering(&identity, view).is_none());
    windows.want(paged, view);
    assert!(windows.wanting());
    let (reading, rect, cancel) = windows.start().expect("a read to start");
    assert_eq!(
        [rect.x0, rect.y0, rect.width, rect.height],
        [view.x0 - WINDOW_MARGIN, 0, 900 + WINDOW_MARGIN, HEIGHT],
        "{test}: the region and its margin, clamped to level 0"
    );
    assert!(windows.start().is_none(), "{test}: one read in flight");
    let read = read_window(&reading, rect, &cancel);
    let window = read.clone().expect("the window reads");
    for channel in 0..3usize {
        for row in [0u32, rect.height - 1] {
            let at = |x: u32| {
                let plane = channel * (window.rect[2] * window.rect[3]) as usize;
                window.planes[plane + (row * window.rect[2] + x) as usize]
            };
            let whole = |x: u32| {
                planes[channel * (WIDTH * HEIGHT) as usize
                    + ((rect.y0 + row) * WIDTH + rect.x0 + x) as usize]
            };
            for x in [0, rect.width / 2, rect.width - 1] {
                assert_eq!(at(x), whole(x), "{test}: channel {channel} ({x}, {row})");
            }
        }
    }
    assert_eq!(
        windows.finish(
            WindowAnswer {
                identity: identity.clone(),
                rect,
                read
            },
            7
        ),
        Ok(true)
    );
    let held = windows
        .covering(&identity, view)
        .expect("the window holds the view");
    assert_eq!(held.stage, (WIDTH, HEIGHT));
    assert_eq!(held.gpu.version(), 7);
    assert!(
        held.gpu.bytes() < u64::from(WIDTH) * u64::from(HEIGHT) * 3 * 4 / 8,
        "{test}: never level 0 whole ({} bytes)",
        held.gpu.bytes()
    );
    assert!(windows.handed().is_some());

    // A pan within the window reads nothing; one beyond it cancels the read in flight.
    let within = Region {
        x0: view.x0 - 100,
        ..view
    };
    windows.want(paged, within);
    assert!(
        windows.start().is_none(),
        "{test}: the window held serves a pan within it"
    );
    let far = Region { x0: 2_000, ..view };
    windows.want(paged, far);
    let (_, first, stale) = windows.start().expect("a read for the far region");
    let further = Region { x0: 6_000, ..view };
    windows.want(paged, further);
    let (_, _, fresh) = windows.start().expect("a read for the further region");
    assert!(
        stale.check().is_err(),
        "{test}: the stale read is cancelled"
    );
    assert!(fresh.check().is_ok());
    assert_eq!(
        windows.finish(
            WindowAnswer {
                identity: identity.clone(),
                rect: first,
                read: Err("cancelled".into())
            },
            8
        ),
        Ok(false),
        "{test}: a stale answer is dropped"
    );
    assert!(
        windows.covering(&identity, view).is_some(),
        "{test}: the held window stays"
    );
    windows.clear();
    assert!(windows.handed().is_none() && !windows.wanting());
    assert!(
        fresh.check().is_err(),
        "{test}: clearing cancels the read in flight"
    );
    owner.stop();
    join.join().unwrap();
    drop(dir);
}
