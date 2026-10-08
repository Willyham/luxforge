//! The desktop's GPU tile worker (`app::gpu_tiles`) on this host's adapter, headless: its reads
//! and its streams against the photo surface's own drawing (`HeadlessSurface`) on another device of
//! the same adapter, and against the reference service (`ReferenceTiles`).
//!
//! - **Reads.** Every read through the worker — of every family on both paths, of the output stage
//!   and of the stage each layer receives, as codes and as linear values — is bit for bit the
//!   pixel the photo surface draws there over the whole stage as one region at full scale, and
//!   the linear value an independent runner draws there, the worker's scratch planes starting
//!   from NaN. Against the reference service the same stages, read at the cells of a 9 × 9 grid,
//!   are within their class's display limit. A neutral pick's 25 points draw one tile, a seed is
//!   the code of the sample input, and the worker keeps nothing of a call's stack once answered.
//! - **The reference, by name.** A read of a stack the GPU cannot plan, a launch that refused the
//!   GPU, an adapter the host does not offer and a lost device are answered by the reference,
//!   naming why, and a stream of them is refused or ended naming it.
//! - **Order and bounds.** A read waits behind at most the one export tile being drawn, and not at
//!   all behind a stream whose encoder has its bands to take; a full queue refuses at once; a
//!   disconnect drops a client's waiting reads and cancels its read being answered.
//! - **Streams.** A stream's bands, stitched, are the whole stage the surface draws, bit for bit;
//!   two streams, on one device and on two, are byte-identical; a cancelled stream stops between
//!   tiles and lets go of what it held.
//! - **Exports.** Through a catalog owner's export lane, as the desktop's launch hands it the
//!   worker: a GPU export, decoded, is within its class's display limit of the reference export
//!   decoded, over every pixel, on generated photographs and the corpus's Presence fixture; two
//!   GPU exports, through one device and through two, are the same bytes; the original is never
//!   changed; and a launch that refused the GPU, or has not named its window's adapter yet, exports
//!   the reference's file naming why.
//!
//! A GPU test with no adapter prints that it was skipped and asserts nothing: it is not GPU
//! evidence.
use super::{
    gpu_plan::{WarpGrid, install_output_encoding, surface_plan_over},
    gpu_tiles::GpuTiles,
    gpu_tiles_tests::{families, gpu_source, host_adapter},
    gpu_window_tests::{HEIGHT, WIDTH, source},
    tasks::call as owner_call,
    testing::{entry, fresh_stack, import_and_adopt},
};
use crate::adapters;
use luxforge_core::{
    AssetId, BASIC_EFFECT, BoundaryFormat, Cancel, ClientId, DETAIL_EFFECT, EffectStage, Error,
    Evaluation, GpuFallback, HostConfig, Layer, ModuleRegistry, OwnerHandle, PRESENCE_EFFECT,
    PreviewSource, Recipe, Region, RenderContext, TilePlan, plan_read, plan_stream_sweeps_at,
    tiles::{
        Answered, BandStream, EXPORT_BANDS_IN_FLIGHT, MaskInputMode, ReadAnswer, ReadPixels,
        ReadStage, ReadValues, ReferenceTiles, TileCall, TileFallback, TileService, TileStatus,
        TileUnavailable,
    },
};
use luxforge_gpu::{
    Derivation, GpuBoundary, GpuPlan, GpuSource, headless::HeadlessSurface, tiles::GPU_TILE_BUDGET,
    tiles::TileEnd, tiles::TilePixels,
};
use luxforge_reference::preview_error::{Class, ciede2000, lab_from_srgb8, statistics_of};
use luxforge_testbase::{Gate, HANG, paths, wait_until};
use serde_json::{Value, json};
use std::sync::{Arc, mpsc};

/// One read: the stage, the rectangle and the values asked for.
type Read = (ReadStage, Region, ReadValues);

/// Every pixel of a stage: a read or a plan of it is the whole stage, clipped.
const WHOLE: Region = Region {
    x0: 0,
    y0: 0,
    width: u32::MAX,
    height: u32::MAX,
};

/// The side the streams of these tests are drawn at: 24 tiles over the 360 × 240 stage, six to a
/// band, the last column and row narrower.
const SIDE: u32 = 64;

fn point(stage: ReadStage, x: u32, y: u32, values: ReadValues) -> Read {
    (
        stage,
        Region {
            x0: x,
            y0: y,
            width: 1,
            height: 1,
        },
        values,
    )
}

/// `count` client identities, which only a catalog owner hands out: one started on a scratch
/// catalog, asked, and stopped.
fn clients(count: usize) -> Vec<ClientId> {
    let catalog = paths::temp_catalog("gpu-tiles-clients");
    let (owner, join) = OwnerHandle::start(&catalog).expect("a catalog owner");
    let clients = (0..count).map(|_| owner.register()).collect();
    owner.stop();
    join.join().expect("the owner stops");
    for path in [paths::wal(&catalog), paths::shm(&catalog), catalog] {
        let _ = std::fs::remove_file(path);
    }
    clients
}

/// `recipe` over `source`, bound as the catalog owner binds a saved entry's stack.
fn stack(source: &PreviewSource, recipe: &Recipe) -> Evaluation {
    let registry = Arc::new(ModuleRegistry::builtin());
    let context = RenderContext::new();
    Evaluation::new(
        registry,
        context,
        source.clone(),
        entry(&AssetId::new(), 1, None),
        recipe.clone(),
        None,
    )
}

/// How layer `layer` of `recipe` receives the stage before it, as the catalog owner asks for it:
/// a colour layer inside its colour run, any other layer, and none past the stack, as the encoded
/// boundary.
fn mode(registry: &ModuleRegistry, recipe: &Recipe, layer: usize) -> MaskInputMode {
    match recipe
        .layers
        .get(layer)
        .and_then(|layer| registry.effect(&layer.effect_id))
    {
        Some((_, effect)) if effect.stage == EffectStage::Color => MaskInputMode::ColourRun,
        _ => MaskInputMode::Boundary,
    }
}

/// What one call on `service` over `stack` answers to `reads`, in order, and the thread that read
/// them.
fn call(
    service: &dyn TileService,
    client: ClientId,
    stack: &Evaluation,
    reads: &[Read],
) -> (Vec<ReadAnswer>, String) {
    let (sender, answers) = mpsc::channel();
    let (done, finished) = mpsc::sync_channel(1);
    let (held, reads) = (stack.clone(), reads.to_vec());
    service.submit(TileCall::caller(
        client,
        Cancel::new(),
        move |tiles, cancel| {
            let mut session = tiles.session(&held, cancel);
            for (stage, rect, values) in reads {
                let _ = sender.send(session.read(stage, rect, values)?);
            }
            Ok(json!(std::thread::current().name()))
        },
        move |result| {
            let _ = done.send(result);
        },
    ));
    let thread = finished
        .recv_timeout(HANG)
        .expect("the call is answered")
        .unwrap_or_else(|error| panic!("the reads: {error:?}"));
    (
        answers.try_iter().collect(),
        thread.as_str().unwrap_or_default().to_owned(),
    )
}

/// Wait for every job queued on `service` before now to be answered, and its figures published:
/// a call that reads nothing, answered after them.
fn settle(service: &dyn TileService, client: ClientId) {
    let (done, finished) = mpsc::sync_channel(1);
    service.submit(TileCall::caller(
        client,
        Cancel::new(),
        |_, _| Ok(Value::Null),
        move |result| {
            let _ = done.send(result);
        },
    ));
    finished
        .recv_timeout(HANG)
        .expect("the call is answered")
        .expect("a call that reads nothing");
}

/// `plan` as the photo surface's plain data over its tile's window of `gpu`, a lens warp's tail
/// through its part of the whole stage's grid, as the worker converts a tile.
fn converted(plan: &TilePlan, gpu: &GpuSource, version: u64) -> GpuPlan {
    let window = plan.tile.window;
    let boundary = GpuBoundary::derived(
        gpu,
        Derivation::Cut {
            origin: (window.x0, window.y0),
        },
        window.width,
        window.height,
        version,
    )
    .expect("a derived boundary");
    let grid = plan.warp().map(|warp| {
        let stage = warp
            .stage_grid(1.0)
            .expect("a stage grid")
            .expect("a lens warp's grid");
        WarpGrid::new(&stage.part(plan.tile.rect).expect("the tile's part"))
    });
    surface_plan_over(
        &plan.plan,
        boundary,
        (window.x0, window.y0),
        grid.as_ref(),
        Some(plan.tile.rect),
    )
    .expect("a runnable plan")
}

/// The whole of `stage` of `stack` drawn as one region at full scale by the photo surface's own
/// drawing, from the window of `gpu` the planner gives it: the plan of the stage, and its codes,
/// row by row.
fn drawn_whole(
    surface: &mut HeadlessSurface,
    gpu: &GpuSource,
    stack: &Evaluation,
    stage: ReadStage,
    version: u64,
) -> (TilePlan, Vec<[u8; 4]>) {
    let plan =
        plan_read(stack, stage, WHOLE).unwrap_or_else(|fallback| panic!("{stage:?}: {fallback:?}"));
    let codes = surface
        .tile(gpu, &converted(&plan, gpu, version))
        .unwrap_or_else(|fallback| panic!("{stage:?}: the surface's drawing: {fallback:?}"));
    (plan, codes)
}

/// A linear value's output code by the core's own thresholds: clamped to `[0, 1]`, NaN taken to
/// 0, the number of thresholds at or below it.
fn code(value: f32) -> u8 {
    let value = if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    };
    luxforge_core::colour::srgb::output_thresholds()
        .partition_point(|threshold| *threshold <= value) as u8
}

/// Every band of `stream` in order, stitched into its output stage's codes, each band checked to
/// start where the last ended and to be at most `side` rows.
fn stitched(stream: BandStream, side: u32) -> Vec<u8> {
    let (mut rgba, mut rows) = (Vec::new(), 0);
    for band in stream {
        let band = band.unwrap_or_else(|error| panic!("a band: {error:?}"));
        assert_eq!(band.y0, rows, "bands in order");
        assert!(band.rows <= side, "a band is one row of tiles");
        rows += band.rows;
        rgba.extend(band.rgba);
    }
    rgba
}

/// The first pixel two stages' codes differ at, and how many differ.
fn first_difference(width: u32, ours: &[u8], theirs: &[u8]) -> Option<String> {
    let differing: Vec<usize> = (0..ours.len() / 4)
        .filter(|index| ours[index * 4..index * 4 + 4] != theirs[index * 4..index * 4 + 4])
        .collect();
    let index = *differing.first()?;
    Some(format!(
        "{} of {} pixels differ, the first at ({}, {}): {:?} against {:?}",
        differing.len(),
        ours.len() / 4,
        index as u32 % width,
        index as u32 / width,
        &ours[index * 4..index * 4 + 4],
        &theirs[index * 4..index * 4 + 4],
    ))
}

/// A gate shut from the start.
fn shut() -> Arc<Gate> {
    let gate = Arc::new(Gate::new());
    gate.shut();
    gate
}

/// Hold `service`'s stream at the gate before its step after the one `held` holds, letting that
/// one through: the gate it is held at now.
fn next_step(service: &GpuTiles, held: &Gate) -> Arc<Gate> {
    let next = shut();
    service.hold_steps(Some(Arc::clone(&next)));
    held.open();
    wait_until("the worker held before the stream's next step", || {
        next.holding()
    });
    next
}

/// Every read of every family on both paths, of every stage a layer receives and of the output
/// stage, as codes and as linear values, at the stage's corners, its middle, two points between
/// and over a small rectangle: the codes are bit for bit those the photo surface's own drawing
/// draws over the whole stage as one region on another device of the same adapter, and the
/// linear values those an independent runner on a third device draws over that region, each as
/// the stage holds it; the worker's scratch starts from NaN, and its thread reads them.
#[test]
fn a_read_through_the_worker_equals_the_headless_surfaces_tile_bit_for_bit() {
    let test = "a_read_through_the_worker_equals_the_headless_surfaces_tile_bit_for_bit";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(
        install_output_encoding(),
        "the surface holds the core's output encoding"
    );
    let window = adapters::open(&backend, &name)
        .unwrap_or_else(|unopened| panic!("{test}: the surface's device: {unopened:?}"));
    let mut surface = HeadlessSurface::new(&window.device, &window.queue);
    let mut runner = crate::adapters::tile_runner(&backend, &name)
        .unwrap_or_else(|refusal| panic!("{test}: the runner: {refusal:?}"));
    let service = GpuTiles::new(Some((backend, name)), false);
    service.poison(true);
    let client = clients(1)[0];
    let registry = ModuleRegistry::builtin();
    let mut versions = 0;
    for (format, path) in [
        (BoundaryFormat::Half, "the byte path"),
        (BoundaryFormat::Float, "the linear path"),
    ] {
        let source = source(format);
        versions += 1;
        let gpu = gpu_source(versions, &source);
        for (family, recipe) in families() {
            let what = format!("{family} on {path}");
            let stack = stack(&source, &recipe);
            let stages = (0..=recipe.layers.len())
                .map(|layer| ReadStage::Before {
                    layer,
                    mode: mode(&registry, &recipe, layer),
                })
                .chain(std::iter::once(ReadStage::Output));
            let mut compared = 0;
            for stage in stages {
                versions += 2;
                let (plan, codes) = drawn_whole(&mut surface, &gpu, &stack, stage, versions - 1);
                let held = plan.tile.window;
                let linear = match runner.run(
                    &converted(&plan, &gpu, versions),
                    &gpu,
                    [held.x0, held.y0, held.width, held.height],
                    TileEnd::Linear,
                ) {
                    Ok(TilePixels::Linear(values)) => values,
                    other => panic!("{what}, {stage:?}: the independent runner: {other:?}"),
                };
                let (width, height) = (plan.size.width, plan.size.height);
                let mut reads = Vec::new();
                for values in [ReadValues::Codes, ReadValues::Linear] {
                    for (x, y) in [
                        (0, 0),
                        (width - 1, height - 1),
                        (width / 2, height / 2),
                        (width / 3, height / 5),
                        (width * 4 / 5, height * 2 / 3),
                    ] {
                        reads.push(point(stage, x, y, values));
                    }
                    reads.push((
                        stage,
                        Region {
                            x0: width / 4,
                            y0: height / 4,
                            width: 7,
                            height: 5,
                        },
                        values,
                    ));
                }
                let (answers, thread) = call(&service, client, &stack, &reads);
                assert_eq!(thread, "luxforge-gpu-tiles", "{what}: the worker reads");
                assert_eq!(answers.len(), reads.len(), "{what}");
                for ((_, rect, values), answer) in reads.iter().zip(&answers) {
                    let at = format!("{what}, {stage:?}, {rect:?} as {values:?}");
                    assert_eq!(answer.answered, Answered::gpu(), "{at}");
                    assert_eq!((answer.stage, answer.rect), (plan.size, *rect), "{at}");
                    for y in rect.y0..rect.y1() {
                        for x in rect.x0..rect.x1() {
                            let index = (y * width + x) as usize;
                            match values {
                                ReadValues::Codes => assert_eq!(
                                    answer.code(x, y),
                                    Some(codes[index]),
                                    "{at}, ({x}, {y}): the surface's code"
                                ),
                                ReadValues::Linear => {
                                    let ReadPixels::Linear(drawn) =
                                        plan.answer(ReadValues::Linear, [linear[index]])
                                    else {
                                        panic!("{at}: linear values answer linear values");
                                    };
                                    assert_eq!(
                                        answer.linear(x, y).map(|value| value.map(f32::to_bits)),
                                        Some(drawn[0].map(f32::to_bits)),
                                        "{at}, ({x}, {y}): the runner's linear value"
                                    );
                                }
                            }
                            compared += 1;
                        }
                    }
                }
            }
            eprintln!(
                "{test}: {what}: {compared} pixels read through the worker over {} stages, bit for \
                 bit the surface's codes and the runner's linear values",
                recipe.layers.len() + 2
            );
        }
    }
    eprintln!("{test}: the worker's figures {:?}", service.figures());
}

/// The output stage and the stage the last layer receives, read through the worker and through
/// the reference service at the cells of a 9 × 9 grid, for every family on both paths: within
/// the family's class's display limit — mean ΔE00, p99 and the signed mean ΔL\*, a worst block
/// having no meaning over scattered points — with each figure printed.
#[test]
fn a_read_is_within_the_display_limit_of_the_reference_service() {
    let test = "a_read_is_within_the_display_limit_of_the_reference_service";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let service = GpuTiles::new(Some((backend, name)), false);
    let reference = ReferenceTiles::new();
    let client = clients(1)[0];
    let registry = ModuleRegistry::builtin();
    for (format, path) in [
        (BoundaryFormat::Half, "the byte path"),
        (BoundaryFormat::Float, "the linear path"),
    ] {
        let source = source(format);
        for (family, recipe) in families() {
            let class = if family.contains("Presence") || family.contains("Detail") {
                Class::Spatial
            } else {
                Class::Pointwise
            };
            let limits = class.limits();
            let stack = stack(&source, &recipe);
            let last = recipe.layers.len() - 1;
            for stage in [
                ReadStage::Output,
                ReadStage::Before {
                    layer: last,
                    mode: mode(&registry, &recipe, last),
                },
            ] {
                let size = plan_read(&stack, stage, WHOLE).expect("a plan").size;
                let reads: Vec<Read> = (0..9)
                    .flat_map(|row| (0..9).map(move |column| (row, column)))
                    .map(|(row, column)| {
                        point(
                            stage,
                            (2 * column + 1) * size.width / 18,
                            (2 * row + 1) * size.height / 18,
                            ReadValues::Codes,
                        )
                    })
                    .collect();
                let (gpu, _) = call(&service, client, &stack, &reads);
                let (cpu, _) = call(&reference, client, &stack, &reads);
                let (mut delta_e, mut delta_l) = (Vec::new(), Vec::new());
                for ((_, rect, _), (gpu, cpu)) in reads.iter().zip(gpu.iter().zip(&cpu)) {
                    assert_eq!(gpu.answered, Answered::gpu(), "{family} on {path}");
                    let (ours, theirs) = (
                        gpu.code(rect.x0, rect.y0).expect("the GPU's code"),
                        cpu.code(rect.x0, rect.y0).expect("the reference's code"),
                    );
                    let ours = lab_from_srgb8([ours[0], ours[1], ours[2]]);
                    let theirs = lab_from_srgb8([theirs[0], theirs[1], theirs[2]]);
                    delta_e.push(ciede2000(theirs, ours));
                    delta_l.push(ours[0] - theirs[0]);
                }
                let figures =
                    statistics_of(reads.len(), 1, &delta_e, &delta_l).expect("the statistics");
                eprintln!(
                    "{test}: {family} on {path}, {stage:?}: {} samples, mean ΔE00 {:.4}, p99 \
                     {:.3}, signed mean ΔL* {:+.4}, max {:.3}, against the {} limits {} / {} / {}",
                    figures.pixels,
                    figures.mean,
                    figures.p99,
                    figures.mean_delta_l,
                    figures.max,
                    class.name(),
                    limits.mean,
                    limits.p99,
                    limits.mean_delta_l,
                );
                assert!(
                    figures.mean <= limits.mean
                        && figures.p99 <= limits.p99
                        && figures.mean_delta_l.abs() <= limits.mean_delta_l,
                    "{family} on {path}, {stage:?}: {figures:?} past the {} limits",
                    class.name()
                );
            }
        }
    }
    reference.stop();
}

/// The neutral picker's patch, the 25 points of a 5 × 5 square read one at a time from its corner
/// before Basic, behind Detail, through one call: one tile drawn, its points the patch one
/// rectangle read answers; and once the call is answered the worker holds nothing of its stack,
/// not even the window of its source the runner kept.
#[test]
fn a_picks_25_points_render_one_window() {
    let test = "a_picks_25_points_render_one_window";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let service = GpuTiles::new(Some((backend, name)), false);
    let client = clients(1)[0];
    let registry = ModuleRegistry::builtin();
    let recipe = Recipe {
        layers: vec![
            Layer::new(
                DETAIL_EFFECT,
                json!({"sharpening": 60.0, "luminance": 30.0}),
            ),
            Layer::new(BASIC_EFFECT, json!({"exposure": 0.3, "contrast": 15.0})),
            Layer::new(PRESENCE_EFFECT, json!({"clarity": 30.0, "dehaze": 15.0})),
        ],
        ..Recipe::default()
    };
    let stage = ReadStage::Before {
        layer: 1,
        mode: mode(&registry, &recipe, 1),
    };
    let (cx, cy) = (WIDTH / 2, HEIGHT / 3);
    let points: Vec<Read> = (cy - 2..=cy + 2)
        .flat_map(|y| (cx - 2..=cx + 2).map(move |x| point(stage, x, y, ReadValues::Codes)))
        .collect();
    let patch = Region {
        x0: cx - 2,
        y0: cy - 2,
        width: 5,
        height: 5,
    };
    for (format, path) in [
        (BoundaryFormat::Half, "the byte path"),
        (BoundaryFormat::Float, "the linear path"),
    ] {
        let stack = stack(&source(format), &recipe);
        settle(&service, client);
        let before = service.figures();
        let (answers, _) = call(&service, client, &stack, &points);
        settle(&service, client);
        let after = service.figures();
        assert_eq!(
            after.tiles - before.tiles,
            1,
            "{path}: one tile for 25 points"
        );
        assert_eq!(
            after.reads - before.reads,
            25,
            "{path}: every point the GPU's"
        );
        // The tile's times, its own and added to the runner's total, as evidence records them.
        assert!(
            after.last.encode_us + after.last.wait_us > 0,
            "{path}: {:?}",
            after.last
        );
        assert_eq!(
            after.total.wait_us - before.total.wait_us,
            after.last.wait_us,
            "{path}: the tile's wait summed"
        );
        let record = after.record();
        assert!(
            record["last_tile"]["wait_ms"].is_f64() && record["tiles_total"]["read_ms"].is_f64(),
            "{path}: {record}"
        );
        let (whole, _) = call(
            &service,
            client,
            &stack,
            &[(stage, patch, ReadValues::Codes)],
        );
        for ((_, rect, _), answer) in points.iter().zip(&answers) {
            assert_eq!(answer.answered, Answered::gpu(), "{path}");
            assert_eq!(
                answer.code(rect.x0, rect.y0),
                whole[0].code(rect.x0, rect.y0),
                "{path}: ({}, {})",
                rect.x0,
                rect.y0
            );
        }
        eprintln!("{test}: {path}: 25 points before Basic behind Detail, one tile: {after:?}");
    }

    // A stack over pixels nothing else holds: once its call is answered, the worker has let go of
    // its evaluation, its source and the window of it the runner kept.
    let (fresh, held) = fresh_stack(&stack(&source(BoundaryFormat::Half), &recipe));
    call(&service, client, &fresh, &points);
    drop(fresh);
    settle(&service, client);
    assert!(
        held.upgrade().is_none(),
        "the worker keeps nothing of an answered call's stack"
    );
    assert_eq!(
        service.figures().in_use,
        0,
        "the runner's window went with the call"
    );
}

/// A colour-limited stroke's seed reads the codes of the stage its masked layer receives, and
/// `mask.sample-input` the linear values of that stage, each in a call of its own: every seed is
/// the core quantizer's code of the sample input there, for every masked layer of every family on
/// both paths.
#[test]
fn a_seed_is_the_code_of_the_sample_input() {
    let test = "a_seed_is_the_code_of_the_sample_input";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let service = GpuTiles::new(Some((backend, name)), false);
    let client = clients(1)[0];
    let registry = ModuleRegistry::builtin();
    let mut seeds = 0;
    for format in [BoundaryFormat::Half, BoundaryFormat::Float] {
        let source = source(format);
        for (family, recipe) in families() {
            let stack = stack(&source, &recipe);
            for (layer, _) in recipe
                .layers
                .iter()
                .enumerate()
                .filter(|(_, layer)| layer.mask.is_some())
            {
                let stage = ReadStage::Before {
                    layer,
                    mode: mode(&registry, &recipe, layer),
                };
                let at = [(17, 23), (WIDTH / 2, HEIGHT / 2), (WIDTH - 9, HEIGHT - 4)];
                let read = |values| -> Vec<Read> {
                    at.iter()
                        .map(|&(x, y)| point(stage, x, y, values))
                        .collect()
                };
                let (codes, _) = call(&service, client, &stack, &read(ReadValues::Codes));
                let (inputs, _) = call(&service, client, &stack, &read(ReadValues::Linear));
                for ((x, y), (seed, input)) in at.into_iter().zip(codes.iter().zip(&inputs)) {
                    let what = format!("{family} on {format:?}, before layer {layer}, ({x}, {y})");
                    assert_eq!(seed.answered, Answered::gpu(), "{what}");
                    assert_eq!(input.answered, Answered::gpu(), "{what}");
                    let value = input.linear(x, y).expect("the sample input");
                    assert_eq!(
                        seed.code(x, y),
                        Some([code(value[0]), code(value[1]), code(value[2]), 255]),
                        "{what}: the seed is the code of the sample input {value:?}"
                    );
                    seeds += 1;
                }
            }
        }
    }
    assert!(seeds > 0, "masked layers were read");
    eprintln!("{test}: {seeds} seeds, each the code of its sample input");
}

/// What the GPU cannot draw is answered by the reference, naming why, with the reference's own
/// pixels: a stack holding a pixel-stage layer, which no GPU program replaces, each read of its
/// call after the first included, and its stream refused at once, while the same stack without it,
/// Dehaze's light and all, the GPU draws; a launch that refused the GPU, which opens nothing; an
/// adapter the host does not offer by that name, which the status then names; and a desktop that
/// named no adapter.
#[test]
fn a_plan_the_gpu_cannot_run_is_answered_by_the_reference_naming_why() {
    let test = "a_plan_the_gpu_cannot_run_is_answered_by_the_reference_naming_why";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let client = clients(1)[0];
    let source = source(BoundaryFormat::Half);
    let (_, recipe) = families()
        .into_iter()
        .find(|(family, _)| *family == "Presence")
        .expect("the Presence family");
    let reference = ReferenceTiles::new();
    // A pixel replaced before Presence, which no GPU program replaces. The read of the stage
    // before that layer, which the GPU could draw, is the reference's too: what remains of a call
    // after a read the GPU cannot draw is the reference's.
    let mut pixel = recipe.clone();
    pixel.layers.insert(0, Layer::pixel(12, 9, [200, 40, 90]));
    let unlit = Evaluation::new(
        Arc::new(ModuleRegistry::developer()),
        RenderContext::new(),
        source.clone(),
        entry(&AssetId::new(), 1, None),
        pixel,
        None,
    );
    let reads = [
        point(ReadStage::Output, 40, 30, ReadValues::Codes),
        point(
            ReadStage::Before {
                layer: 0,
                mode: MaskInputMode::Boundary,
            },
            40,
            30,
            ReadValues::Codes,
        ),
    ];
    let service = GpuTiles::new(Some((backend.clone(), name.clone())), false);
    let why = TileFallback::Plan(GpuFallback::PixelStage { layer: 0 });
    assert_eq!(
        service.stream(&unlit, &Cancel::new()).err(),
        Some(why.clone()),
        "a stream of it is refused at once"
    );
    let (answers, _) = call(&service, client, &unlit, &reads);
    let (expected, _) = call(&reference, client, &unlit, &reads);
    for (answer, expected) in answers.iter().zip(&expected) {
        assert_eq!(answer.answered, Answered::reference(Some(why.clone())));
        assert_eq!(answer.pixels, expected.pixels, "the reference's own pixels");
    }
    assert_eq!(service.status(), TileStatus::Gpu, "the runner itself draws");
    // Without it the GPU draws the stack, Presence's Dehaze light computed by the runner.
    let held = stack(&source, &recipe);
    assert!(
        recipe.layers[0].payload["dehaze"]
            .as_f64()
            .is_some_and(|dehaze| dehaze != 0.0),
        "the Presence family reads a light"
    );
    let (answers, _) = call(&service, client, &held, &reads);
    assert!(
        answers
            .iter()
            .all(|answer| answer.answered == Answered::gpu()),
        "the GPU draws a stack that reads a light"
    );
    let (expected, _) = call(&reference, client, &held, &reads);

    // A launch that refused the GPU opens nothing, and says so.
    let refused = GpuTiles::new(Some((backend.clone(), name.clone())), true);
    let refusal = TileFallback::Unavailable(TileUnavailable::Refused);
    assert_eq!(
        refused.status(),
        TileStatus::Reference(Some(refusal.clone()))
    );
    let (answers, _) = call(&refused, client, &held, &reads[..1]);
    assert_eq!(
        answers[0].answered,
        Answered::reference(Some(refusal.clone()))
    );
    assert_eq!(answers[0].pixels, expected[0].pixels);
    assert_eq!(refused.stream(&held, &Cancel::new()).err(), Some(refusal));
    settle(&refused, client);
    assert_eq!(refused.figures().adapter, None, "nothing was opened");

    // An adapter the host does not offer by that name is refused by name, never replaced.
    let mismatched = GpuTiles::new(Some((backend, "an adapter no host offers".into())), false);
    assert_eq!(
        mismatched.status(),
        TileStatus::Gpu,
        "not known until a call opens the runner"
    );
    let (answers, _) = call(&mismatched, client, &held, &reads[..1]);
    let mismatch = TileFallback::Unavailable(TileUnavailable::AdapterMismatch);
    assert_eq!(
        answers[0].answered,
        Answered::reference(Some(mismatch.clone()))
    );
    assert_eq!(answers[0].pixels, expected[0].pixels);
    assert_eq!(mismatched.status(), TileStatus::Reference(Some(mismatch)));
    settle(&mismatched, client);
    let figures = mismatched.figures();
    assert!(
        figures
            .refusal
            .as_deref()
            .is_some_and(|refusal| refusal.contains("an adapter no host offers")),
        "the refusal names the adapter asked for: {figures:?}"
    );

    // A desktop that named no adapter.
    assert_eq!(
        GpuTiles::new(None, false).status(),
        TileStatus::Reference(Some(TileFallback::Unavailable(TileUnavailable::NoAdapter)))
    );
    reference.stop();
}

/// A read submitted while the worker is held before an export's step is answered once that step
/// is taken, before the next: the stream has at most one tile in flight then, so the read waits
/// behind at most that tile and the one the step read back; a stream whose two bands its encoder
/// has not taken draws nothing more and holds no tile in flight, and a read then waits behind no
/// tile at all; and once the encoder takes a band, the stream draws on.
#[test]
fn an_interactive_read_waits_behind_at_most_two_export_tiles() {
    let test = "an_interactive_read_waits_behind_at_most_two_export_tiles";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let service = Arc::new(GpuTiles::new(Some((backend, name)), false));
    service.draw_streams_at(vec![SIDE]);
    let client = clients(1)[0];
    let (_, recipe) = families()
        .into_iter()
        .find(|(family, _)| *family == "Presence")
        .expect("the Presence family");
    let stack = stack(&source(BoundaryFormat::Half), &recipe);
    // A read whose call notes how many tiles the worker had drawn when it began answering it.
    let observed = |service: &Arc<GpuTiles>| {
        let (seen, noted) = mpsc::sync_channel(1);
        let (done, finished) = mpsc::sync_channel(1);
        let (observer, held) = (Arc::clone(service), stack.clone());
        service.submit(TileCall::caller(
            client,
            Cancel::new(),
            move |tiles, cancel| {
                let figures = observer.figures();
                let _ = seen.send((figures.tiles, figures.in_flight));
                let (stage, rect, values) = point(ReadStage::Output, 50, 60, ReadValues::Codes);
                let answer = tiles.session(&held, cancel).read(stage, rect, values)?;
                Ok(json!(answer.answered == Answered::gpu()))
            },
            move |result| {
                let _ = done.send(result);
            },
        ));
        (noted, finished)
    };

    // Held before the stream's beginning, then before its first tile.
    let begin = shut();
    service.hold_steps(Some(Arc::clone(&begin)));
    let mut bands = service.stream(&stack, &Cancel::new()).expect("a stream");
    wait_until("the worker held before the stream begins", || {
        begin.holding()
    });
    let first = next_step(&service, &begin);
    let drawn = service.figures().tiles;
    let (noted, finished) = observed(&service);
    service.hold_steps(None);
    first.open();
    let (tiles, in_flight) = noted.recv_timeout(HANG).expect("the read begins");
    assert!(
        tiles <= drawn + 1 && in_flight <= 1,
        "the read waited behind the step being taken and at most one tile in flight: {tiles} \
         read back of {drawn} before, {in_flight} in flight"
    );
    assert_eq!(
        finished
            .recv_timeout(HANG)
            .expect("answered")
            .expect("read"),
        json!(true),
        "the GPU drew the read"
    );

    // The encoder takes no band: once two wait for it the stream draws nothing more.
    wait_until("two bands wait for the encoder", || {
        service.figures().bands == EXPORT_BANDS_IN_FLIGHT as u64
    });
    settle(&*service, client);
    let parked = service.figures();
    let (noted, finished) = observed(&service);
    assert_eq!(
        noted.recv_timeout(HANG).expect("the read begins"),
        (parked.tiles, 0),
        "a read waits behind no tile of a stream with no room"
    );
    finished
        .recv_timeout(HANG)
        .expect("answered")
        .expect("read");
    settle(&*service, client);
    assert_eq!(
        service.figures().bands,
        parked.bands,
        "the stream draws no band while two wait"
    );

    // The encoder takes one: the stream draws its next band.
    let band = bands.next().expect("a band").expect("the first band");
    assert_eq!(band.y0, 0);
    wait_until("a third band", || {
        service.figures().bands == EXPORT_BANDS_IN_FLIGHT as u64 + 1
    });
    eprintln!("{test}: {:?}", service.figures());
    drop(bands);
}

/// The stage of the staged streams' photograph: large enough that Presence's reach, which grows
/// with the stage, passes the sweep split, so a stack of two Presence layers is drawn in sweeps.
const STAGED: (u32, u32) = (1600, 1000);

/// The side the staged streams' sweeps are drawn at: several tiles a sweep, each sweep's last row
/// and column narrower.
const STAGED_SIDE: u32 = 384;

/// The photograph on either path at [`STAGED`]: detail at every scale, a JPEG's codes or a RAW's
/// linear planes with values past white and below black.
fn staged_source(format: BoundaryFormat) -> PreviewSource {
    let (width, height) = STAGED;
    let value = |x: u32, y: u32, channel: u32| {
        let (x, y) = (f64::from(x), f64::from(y));
        let wave = (x * 0.031 + y * 0.017 + f64::from(channel)).sin() * 0.3
            + (x * 0.0023 - y * 0.0041 * f64::from(channel + 1)).cos() * 0.15;
        (0.45 + wave + ((x * 7.0 + y * 3.0) % 11.0) / 60.0).clamp(0.0, 1.0)
    };
    match format {
        BoundaryFormat::Half => {
            let mut rgba = Vec::with_capacity((width * height * 4) as usize);
            for y in 0..height {
                for x in 0..width {
                    for channel in 0..3 {
                        rgba.push((value(x, y, channel) * 255.0).round() as u8);
                    }
                    rgba.push(255);
                }
            }
            PreviewSource::Jpeg(luxforge_core::SourceImage {
                width,
                height,
                rgba: rgba.into(),
                fingerprint: "sha256:gpu-staged".into(),
                orientation: 1,
                capture: Default::default(),
            })
        }
        BoundaryFormat::Float => {
            let mut planes = Vec::with_capacity((width * height * 3) as usize);
            for channel in 0..3 {
                for y in 0..height {
                    for x in 0..width {
                        planes.push((value(x, y, channel) * 1.4 - 0.05) as f32);
                    }
                }
            }
            PreviewSource::Raw {
                image: luxforge_core::LinearImage::new(width, height, planes).expect("an image"),
                settings: luxforge_core::LinearSettings::default(),
            }
        }
    }
}

/// The stacks a stream draws in staged sweeps at [`STAGED`]: two Presence layers, the second
/// masked by a radial, and the same before a lens warp.
fn staged_families() -> Vec<(&'static str, Recipe)> {
    use super::gpu_tiles_tests::{radial, recipe};
    let presence = |values| Layer::new(PRESENCE_EFFECT, values);
    let layers = vec![
        presence(json!({"texture": 30.0, "clarity": 60.0, "dehaze": 30.0})),
        presence(json!({"texture": 20.0, "clarity": 40.0})),
    ];
    let mut warped = layers.clone();
    warped.push(luxforge_core::qualification::lens_layer(-0.06, STAGED));
    vec![
        (
            "a masked Presence after Presence",
            recipe(layers, vec![(1, radial())]),
        ),
        (
            "the same before a lens warp",
            recipe(warped, vec![(1, radial())]),
        ),
    ]
}

/// A stack whose layers together reach past the sweep split, streamed in staged sweeps — every
/// sweep before the last drawn into the runner's stage textures, the last's tiles cut from them —
/// is bit for bit the whole output stage the photo surface's own drawing draws as one chained
/// region on another device of the same adapter, on both paths, the worker's scratch starting
/// from NaN; twice on one worker it is the same bytes; and once it ends the runner holds no stage
/// texture.
#[test]
fn a_staged_stream_is_the_whole_stage_render_bit_for_bit() {
    let test = "a_staged_stream_is_the_whole_stage_render_bit_for_bit";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let window = adapters::open(&backend, &name)
        .unwrap_or_else(|unopened| panic!("{test}: the surface's device: {unopened:?}"));
    let mut surface = HeadlessSurface::new(&window.device, &window.queue);
    let service = GpuTiles::new(Some((backend, name)), false);
    service.poison(true);
    service.draw_streams_at(vec![STAGED_SIDE]);
    let client = clients(1)[0];
    let budget = GPU_TILE_BUDGET - super::gpu_tiles::STREAM_READ_RESERVE;
    let mut versions = 100;
    for (format, path) in [
        (BoundaryFormat::Half, "the byte path"),
        (BoundaryFormat::Float, "the linear path"),
    ] {
        let source = staged_source(format);
        versions += 1;
        let gpu = gpu_source(versions, &source);
        for (family, recipe) in staged_families() {
            let what = format!("{family} on {path}");
            let stack = stack(&source, &recipe);
            let staging = plan_stream_sweeps_at(&stack, budget, &[STAGED_SIDE])
                .unwrap_or_else(|fallback| panic!("{what}: {fallback:?}"));
            let Some(sweeps) = staging.sweeps() else {
                panic!("{what}: planned chained, {staging:?}");
            };
            let tiles: Vec<usize> = sweeps
                .sweeps
                .iter()
                .map(|sweep| sweep.tiles.len())
                .collect();
            versions += 1;
            let (plan, codes) =
                drawn_whole(&mut surface, &gpu, &stack, ReadStage::Output, versions);
            settle(&service, client);
            let before = service.figures();
            let stream = |service: &GpuTiles| {
                let bands = service
                    .stream(&stack, &Cancel::new())
                    .unwrap_or_else(|fallback| panic!("{what}: {fallback:?}"));
                assert_eq!(bands.answered(), &Answered::gpu(), "{what}");
                stitched(bands, STAGED_SIDE)
            };
            let rgba = stream(&service);
            assert_eq!(rgba.len(), codes.len() * 4, "{what}: the whole stage");
            if let Some(difference) = first_difference(plan.size.width, &rgba, &codes.concat()) {
                panic!("{what}: the staged stream against the surface's region: {difference}");
            }
            assert!(stream(&service) == rgba, "{what}: twice on one device");
            settle(&service, client);
            let after = service.figures();
            assert_eq!(
                after.staged - before.staged,
                2,
                "{what}: both streams staged"
            );
            assert_eq!(
                (after.stage_bytes, after.in_use, after.in_flight),
                (0, 0, 0),
                "{what}: nothing held once it ends"
            );
            eprintln!(
                "{test}: {what}: {} x {} in {} sweeps of {tiles:?} tiles, bit for bit the \
                 surface's chained region; peak {} B",
                plan.size.width,
                plan.size.height,
                sweeps.sweeps.len(),
                after.peak
            );
        }
    }
}

/// Every family on both paths streamed in tiles of [`SIDE`], the worker's scratch starting from
/// NaN, two tiles in flight and each band's window of the source uploaded once: its bands,
/// stitched, are bit for bit the whole output stage the photo surface's own drawing draws as one
/// region on another device of the same adapter, so its tiles carry no seam.
#[test]
fn a_stream_is_the_whole_stage_render_bit_for_bit() {
    let test = "a_stream_is_the_whole_stage_render_bit_for_bit";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let window = adapters::open(&backend, &name)
        .unwrap_or_else(|unopened| panic!("{test}: the surface's device: {unopened:?}"));
    let mut surface = HeadlessSurface::new(&window.device, &window.queue);
    let service = GpuTiles::new(Some((backend, name)), false);
    service.poison(true);
    service.draw_streams_at(vec![SIDE]);
    let client = clients(1)[0];
    let mut versions = 0;
    for (format, path) in [
        (BoundaryFormat::Half, "the byte path"),
        (BoundaryFormat::Float, "the linear path"),
    ] {
        let source = source(format);
        versions += 1;
        let gpu = gpu_source(versions, &source);
        for (family, recipe) in families() {
            let what = format!("{family} on {path}");
            let stack = stack(&source, &recipe);
            versions += 1;
            let (plan, codes) =
                drawn_whole(&mut surface, &gpu, &stack, ReadStage::Output, versions);
            settle(&service, client);
            let before = service.figures();
            let bands = service
                .stream(&stack, &Cancel::new())
                .unwrap_or_else(|fallback| panic!("{what}: {fallback:?}"));
            assert_eq!(bands.answered(), &Answered::gpu(), "{what}");
            let rgba = stitched(bands, SIDE);
            assert_eq!(rgba.len(), codes.len() * 4, "{what}: the whole stage");
            if let Some(difference) = first_difference(plan.size.width, &rgba, &codes.concat()) {
                panic!("{what}: the stream against the surface's region: {difference}");
            }
            // Each band's window of the source uploaded once for all of its tiles, and nothing
            // held once the stream ends.
            settle(&service, client);
            let after = service.figures();
            assert_eq!(
                after.uploads - before.uploads,
                u64::from(plan.size.height.div_ceil(SIDE)),
                "{what}: one upload a band"
            );
            assert_eq!((after.in_flight, after.in_use), (0, 0), "{what}");
            eprintln!(
                "{test}: {what}: {} x {} in tiles of {SIDE}, bit for bit the surface's region",
                plan.size.width, plan.size.height
            );
        }
    }
    eprintln!("{test}: the worker's figures {:?}", service.figures());
}

/// The same export streamed twice on one worker, and once on a second worker's device of the
/// same adapter, in tiles of [`SIDE`] and at the longest side: byte-identical, for the heavier
/// families on both paths.
#[test]
fn two_streams_are_byte_identical() {
    let test = "two_streams_are_byte_identical";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let adapter = Some((backend, name));
    for (sides, tiles) in [
        (Some(vec![SIDE]), "tiles of 64"),
        (None, "the longest side"),
    ] {
        let (one, two) = (
            GpuTiles::new(adapter.clone(), false),
            GpuTiles::new(adapter.clone(), false),
        );
        if let Some(sides) = sides {
            one.draw_streams_at(sides.clone());
            two.draw_streams_at(sides);
        }
        for (format, path) in [
            (BoundaryFormat::Half, "the byte path"),
            (BoundaryFormat::Float, "the linear path"),
        ] {
            let source = source(format);
            for (family, recipe) in families().into_iter().filter(|(family, _)| {
                [
                    "Presence after Detail",
                    "a lens warp",
                    "a masked Presence after Basic",
                ]
                .contains(family)
            }) {
                let what = format!("{family} on {path} in {tiles}");
                let stack = stack(&source, &recipe);
                let stream = |service: &GpuTiles| {
                    stitched(
                        service.stream(&stack, &Cancel::new()).expect("a stream"),
                        u32::MAX,
                    )
                };
                let first = stream(&one);
                assert!(
                    first.iter().any(|byte| *byte != first[0]),
                    "{what}: a picture"
                );
                assert!(stream(&one) == first, "{what}: twice on one device");
                assert!(stream(&two) == first, "{what}: on a second device");
                eprintln!("{test}: {what}: byte-identical three times, on two devices");
            }
        }
    }
}

/// A stream held before its second tile and cancelled there draws nothing more: its encoder reads
/// the cancellation and then the end, the worker lets go of its tile in flight, unread, of the
/// window of the source and the slot its runner held and of the stack itself, and its figures say
/// so.
#[test]
fn a_cancelled_stream_stops_between_tiles_and_frees_its_slots() {
    let test = "a_cancelled_stream_stops_between_tiles_and_frees_its_slots";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let service = GpuTiles::new(Some((backend, name)), false);
    service.draw_streams_at(vec![SIDE]);
    let client = clients(1)[0];
    let (_, recipe) = families()
        .into_iter()
        .find(|(family, _)| *family == "Presence after Detail")
        .expect("the family");
    let (stack, held) = fresh_stack(&stack(&source(BoundaryFormat::Half), &recipe));
    let cancel = Cancel::new();
    let begin = shut();
    service.hold_steps(Some(Arc::clone(&begin)));
    let mut bands = service.stream(&stack, &cancel).expect("a stream");
    drop(stack);
    wait_until("the worker held before the stream begins", || {
        begin.holding()
    });
    let first = next_step(&service, &begin);
    let second = next_step(&service, &first);
    let drawing = service.figures();
    assert_eq!(
        (drawing.tiles, drawing.in_flight),
        (0, 1),
        "one tile submitted and in flight"
    );
    assert!(
        drawing.in_use > 0,
        "the runner holds the band's window and the tile"
    );

    cancel.cancel();
    service.hold_steps(None);
    second.open();
    let ended = bands.next().expect("the end").expect_err("cancelled");
    assert_eq!(ended.kind.code(), "cancelled");
    assert!(bands.next().is_none(), "nothing after the cancellation");
    settle(&service, client);
    let after = service.figures();
    assert_eq!(after.tiles, drawing.tiles, "no tile after the cancellation");
    assert_eq!(after.bands, 0);
    assert_eq!(after.in_flight, 0, "the tile in flight let go unread");
    assert_eq!(after.in_use, 0, "the runner's window and slot let go");
    assert!(
        held.upgrade().is_none(),
        "the worker holds nothing of a cancelled stream's stack"
    );
    eprintln!("{test}: {after:?}");
}

/// A staged stream drawing its first sweep into its stage textures holds them charged, keeps a read
/// waiting behind at most two of its tiles — the step being taken and at most one in flight — and,
/// cancelled, lets go of them with the rest of what its runner held.
#[test]
fn a_staged_stream_holds_reads_to_two_tiles_and_frees_its_stages_when_cancelled() {
    let test = "a_staged_stream_holds_reads_to_two_tiles_and_frees_its_stages_when_cancelled";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let service = Arc::new(GpuTiles::new(Some((backend, name)), false));
    service.draw_streams_at(vec![STAGED_SIDE]);
    let client = clients(1)[0];
    let (_, recipe) = staged_families().swap_remove(0);
    let stack = stack(&staged_source(BoundaryFormat::Half), &recipe);
    let cancel = Cancel::new();
    let begin = shut();
    service.hold_steps(Some(Arc::clone(&begin)));
    let mut bands = service.stream(&stack, &cancel).expect("a stream");
    wait_until("the worker held before the stream begins", || {
        begin.holding()
    });
    let first = next_step(&service, &begin);
    let second = next_step(&service, &first);
    let drawing = service.figures();
    assert_eq!(drawing.staged, 1, "the stream is staged");
    assert!(
        drawing.stage_bytes > 0 && drawing.in_use >= drawing.stage_bytes,
        "its stage textures charged while it draws: {drawing:?}"
    );
    assert_eq!(drawing.bands, 0, "no band before the last sweep");

    // A read submitted while the worker is held before the sweep's next step.
    let (seen, noted) = mpsc::sync_channel(1);
    let (done, finished) = mpsc::sync_channel(1);
    let (observer, held) = (Arc::clone(&service), stack.clone());
    service.submit(TileCall::caller(
        client,
        Cancel::new(),
        move |tiles, cancel| {
            let figures = observer.figures();
            let _ = seen.send((figures.tiles, figures.in_flight, figures.stage_bytes));
            let (stage, rect, values) = point(ReadStage::Output, 50, 60, ReadValues::Codes);
            let answer = tiles.session(&held, cancel).read(stage, rect, values)?;
            Ok(json!(answer.answered == Answered::gpu()))
        },
        move |result| {
            let _ = done.send(result);
        },
    ));
    service.hold_steps(None);
    second.open();
    let (tiles, in_flight, stage_bytes) = noted.recv_timeout(HANG).expect("the read begins");
    assert!(
        tiles <= drawing.tiles + 1 && in_flight <= 1,
        "the read waited behind the step being taken and at most one tile in flight: {tiles} \
         read back of {} before, {in_flight} in flight",
        drawing.tiles
    );
    assert!(stage_bytes > 0, "the stages held across the read");
    assert_eq!(
        finished
            .recv_timeout(HANG)
            .expect("answered")
            .expect("read"),
        json!(true),
        "the GPU drew the read"
    );

    // Cancelled wherever it has reached: its encoder takes no band, so it cannot have ended.
    cancel.cancel();
    let ended = loop {
        match bands.next().expect("the end") {
            Ok(_) => {}
            Err(ended) => break ended,
        }
    };
    assert_eq!(ended.kind.code(), "cancelled");
    assert!(bands.next().is_none(), "nothing after the cancellation");
    settle(&*service, client);
    let after = service.figures();
    assert_eq!(
        (after.stage_bytes, after.in_use, after.in_flight),
        (0, 0, 0),
        "the stage textures let go with the rest"
    );
    eprintln!("{test}: {drawing:?} then {after:?}");
}

/// A stream whose device is lost before a tile ends naming `device-lost` in its error's data, the
/// export lane's cue to render it again with the reference; from then on the status names it, a
/// read is the reference's naming it, and a stream is refused at once.
#[test]
fn a_lost_device_ends_the_stream_with_device_lost() {
    let test = "a_lost_device_ends_the_stream_with_device_lost";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let service = GpuTiles::new(Some((backend, name)), false);
    service.draw_streams_at(vec![SIDE]);
    let client = clients(1)[0];
    let (_, recipe) = families()
        .into_iter()
        .find(|(family, _)| *family == "a colour stack")
        .expect("the family");
    let stack = stack(&source(BoundaryFormat::Half), &recipe);
    let begin = shut();
    service.hold_steps(Some(Arc::clone(&begin)));
    let mut bands = service.stream(&stack, &Cancel::new()).expect("a stream");
    wait_until("the worker held before the stream begins", || {
        begin.holding()
    });
    let first = next_step(&service, &begin);
    service.lose_device();
    service.hold_steps(None);
    first.open();

    let ended = bands
        .next()
        .expect("the end")
        .expect_err("the device was lost");
    let data = ended.data.as_deref().cloned().unwrap_or_default();
    assert_eq!(data["fallback"], "tiles-unavailable", "{ended:?}");
    assert_eq!(data["unavailable"], "device-lost", "{ended:?}");
    assert!(bands.next().is_none());
    let lost = TileFallback::Unavailable(TileUnavailable::DeviceLost);
    assert_eq!(service.status(), TileStatus::Reference(Some(lost.clone())));
    let (answers, _) = call(
        &service,
        client,
        &stack,
        &[point(ReadStage::Output, 3, 3, ReadValues::Codes)],
    );
    assert_eq!(answers[0].answered, Answered::reference(Some(lost.clone())));
    assert_eq!(service.stream(&stack, &Cancel::new()).err(), Some(lost));
    eprintln!("{test}: {:?}", service.figures());
}

/// A call of `client` answering `value`, which reads nothing, and its answer.
fn plain(client: ClientId, value: Value) -> (TileCall, mpsc::Receiver<Result<Value, Error>>) {
    let (done, answer) = mpsc::sync_channel(1);
    (
        TileCall::caller(
            client,
            Cancel::new(),
            move |_, _| Ok(value),
            move |result| {
                let _ = done.send(result);
            },
        ),
        answer,
    )
}

/// No thread before the first call; past the queue's capacity a call is refused at once with
/// `resource-limit` on its own reply while the worker is held, and the calls queued before it are
/// answered once it is released, in order.
#[test]
fn a_full_queue_refuses_at_once_with_resource_limit() {
    let service = GpuTiles::with_capacity(2, None, false);
    assert!(!service.started(), "no thread before the first call");
    let gate = shut();
    service.hold_calls(Some(Arc::clone(&gate)));
    let client = clients(1)[0];
    let (first, first_answer) = plain(client, json!(1));
    service.submit(first);
    gate.wait_reached(1, "the worker, with the first call");
    assert!(service.started());
    let (second, second_answer) = plain(client, json!(2));
    let (third, third_answer) = plain(client, json!(3));
    service.submit(second);
    service.submit(third);
    assert_eq!(service.waiting(), 2);
    let (fourth, fourth_answer) = plain(client, json!(4));
    service.submit(fourth);
    assert_eq!(
        fourth_answer
            .recv_timeout(HANG)
            .expect("refused at once")
            .unwrap_err()
            .kind
            .code(),
        "resource-limit"
    );
    gate.open();
    for (answer, value) in [(first_answer, 1), (second_answer, 2), (third_answer, 3)] {
        assert_eq!(answer.recv_timeout(HANG).unwrap().unwrap(), json!(value));
    }
}

/// A disconnect drops that client's waiting calls, whose replies close unanswered, and keeps the
/// others'; a disconnect of the client whose call is being answered cancels it before it reads.
#[test]
fn a_disconnect_drops_a_clients_waiting_reads() {
    let service = GpuTiles::with_capacity(4, None, false);
    let gate = shut();
    service.hold_calls(Some(Arc::clone(&gate)));
    let ids = clients(3);
    let (first, first_answer) = plain(ids[0], json!(1));
    service.submit(first);
    gate.wait_reached(1, "the worker, with the first call");
    let (second, second_answer) = plain(ids[1], json!(2));
    let (third, third_answer) = plain(ids[2], json!(3));
    service.submit(second);
    service.submit(third);
    assert_eq!(service.waiting(), 2);
    service.disconnect(ids[1]);
    assert_eq!(service.waiting(), 1);
    assert!(
        second_answer.recv_timeout(HANG).is_err(),
        "a disconnected client's call is dropped, not answered"
    );
    service.disconnect(ids[0]);
    gate.open();
    assert_eq!(
        first_answer
            .recv_timeout(HANG)
            .unwrap()
            .unwrap_err()
            .kind
            .code(),
        "cancelled"
    );
    assert_eq!(third_answer.recv_timeout(HANG).unwrap().unwrap(), json!(3));
}

/// The side the exports of these tests are streamed at: several bands of several tiles each.
const EXPORT_SIDE: u32 = 256;

/// A catalog owner whose export lane streams through `tiles`, as the desktop's launch starts it,
/// over a photograph written as a JPEG into a directory of its own and imported.
struct Exports {
    owner: OwnerHandle,
    join: Option<std::thread::JoinHandle<()>>,
    client: ClientId,
    dir: std::path::PathBuf,
    original: std::path::PathBuf,
    asset: AssetId,
}

impl Exports {
    fn new(name: &str, tiles: Arc<GpuTiles>, photograph: &image::RgbImage) -> Self {
        let dir = paths::temp_dir(&format!("gpu-export-{name}"));
        let original = dir.join("original.jpg");
        let file = std::io::BufWriter::new(std::fs::File::create(&original).unwrap());
        image::codecs::jpeg::JpegEncoder::new_with_quality(file, 95)
            .encode_image(photograph)
            .expect("the photograph is written");
        let (owner, join) = OwnerHandle::start_with_host(
            &dir.join("catalog.sqlite"),
            Arc::new(ModuleRegistry::builtin()),
            HostConfig {
                tiles: Some(tiles),
                ..HostConfig::unconfigured()
            },
        )
        .expect("a catalog owner");
        let client = owner.register();
        let asset = import_and_adopt(&owner, client, &original);
        Self {
            owner,
            join: Some(join),
            client,
            dir,
            original,
            asset,
        }
    }

    /// Commit `method` with `params` as the asset's next entry.
    fn edit(&self, method: &str, mut params: Value) {
        let (state, _) = owner_call(
            &self.owner,
            self.client,
            "asset.state",
            json!({"asset_id": self.asset}),
        )
        .expect("the asset's state");
        let revision = state["revision"].clone();
        params["asset_id"] = json!(self.asset);
        params["mutation"] = json!({
            "expected_revision": revision,
            "request_id": format!("{method}-{revision}"),
            "actor": "test",
        });
        owner_call(&self.owner, self.client, method, params)
            .unwrap_or_else(|error| panic!("{method}: {error}"));
    }

    /// Export the current entry to `name`, through the reference renderer when `reference` asks
    /// for it, and wait for its job to end ready: its result, and the file's bytes.
    fn export(&self, name: &str, reference: bool) -> (Value, Vec<u8>) {
        let destination = self.dir.join(name);
        let mut params = json!({
            "asset_id": self.asset,
            "destination": destination,
            "mutation": {"request_id": format!("export-{name}"), "actor": "test"},
        });
        if reference {
            params["reference"] = json!(true);
        }
        let (queued, _) = owner_call(&self.owner, self.client, "export.jpeg", params)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let job = queued["job_id"].clone();
        let read = luxforge_testbase::wait_for("an export to end", || {
            let (read, _) =
                owner_call(&self.owner, self.client, "job.read", json!({"job_id": job}))
                    .expect("the job");
            (!matches!(read["status"].as_str(), Some("queued" | "running"))).then_some(read)
        });
        assert_eq!(read["status"], "ready", "{name}: {read}");
        let bytes = std::fs::read(&destination).expect("the written file");
        (read["result"].clone(), bytes)
    }

    /// The original's bytes as they are now.
    fn original(&self) -> Vec<u8> {
        std::fs::read(&self.original).expect("the original")
    }
}

impl Drop for Exports {
    fn drop(&mut self) {
        self.owner.stop();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A worker on this host's adapter `adapter` streaming at [`EXPORT_SIDE`].
fn exporter(adapter: &(String, String)) -> Arc<GpuTiles> {
    let worker = Arc::new(GpuTiles::new(Some(adapter.clone()), false));
    worker.draw_streams_at(vec![EXPORT_SIDE]);
    worker
}

/// A generated photograph of `width` × `height`: smooth gradients in every channel, a hard
/// diagonal edge, fine sinusoidal texture and a flat grey patch, so every kind of step of a stack
/// has something to act on.
fn generated(width: u32, height: u32) -> image::RgbImage {
    image::RgbImage::from_fn(width, height, |x, y| {
        if x < width / 6 && y > height * 2 / 3 {
            return image::Rgb([128, 128, 128]);
        }
        let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
        let texture = 18.0 * ((x as f32 * 0.9).sin() * (y as f32 * 0.7).cos());
        let edge = if u > 0.6 + (v - 0.5) * 0.4 { 40.0 } else { 0.0 };
        let code = |base: f32| (base + texture + edge).clamp(0.0, 255.0).round() as u8;
        image::Rgb([
            code(30.0 + 190.0 * u),
            code(40.0 + 170.0 * v),
            code(200.0 - 150.0 * u * v),
        ])
    })
}

/// The corpus's Presence fixture as its generator draws it (`cargo xtask generate-fixtures`): a
/// smooth gradient, a hard step edge, a low-amplitude checker and a flat grey, one in each
/// quadrant.
fn presence_fixture() -> image::RgbImage {
    let (width, height) = (1440u32, 960u32);
    let (hw, hh) = (width / 2, height / 2);
    image::RgbImage::from_fn(width, height, |x, y| {
        let level = match (x < hw, y < hh) {
            (true, true) => (40.0 + x as f32 / hw as f32 * (255.0 - 40.0)).round() as u8,
            (false, true) if x - hw < hw / 2 => 70,
            (false, true) => 210,
            (true, false) if (x / 4 + (y - hh) / 4).is_multiple_of(2) => 118,
            (true, false) => 138,
            (false, false) => 128,
        };
        image::Rgb([level; 3])
    })
}

/// `candidate` against `reference`, two JPEG files each decoded independently, by the preview
/// error statistics over every pixel.
fn decoded_against(
    candidate: &[u8],
    reference: &[u8],
) -> luxforge_reference::preview_error::Statistics {
    let decode = |bytes: &[u8]| {
        image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg)
            .expect("an export decodes")
            .to_rgb8()
    };
    let (candidate, reference) = (decode(candidate), decode(reference));
    assert_eq!(candidate.dimensions(), reference.dimensions());
    let (width, height) = reference.dimensions();
    let rgb = luxforge_reference::preview_error::Rgb8::new;
    luxforge_reference::preview_error::compare(
        rgb(width, height, candidate.as_raw()).unwrap(),
        rgb(width, height, reference.as_raw()).unwrap(),
        [0, 0, width, height],
    )
    .expect("the statistics")
}

/// A GPU export through the export lane, its tiles streamed by the worker on this host's adapter,
/// is within its class's display limit of the reference export, both files decoded independently
/// and compared over every pixel: a colour stack by the pointwise limits and Presence by the
/// spatial limits, on a generated photograph and on the corpus's Presence fixture. Presence's
/// Dehaze light is computed by the worker's runner from the whole stage before any render of the
/// stack, so the GPU exports it at once.
#[test]
fn a_gpu_export_is_within_the_display_limit_of_the_reference_export() {
    let test = "a_gpu_export_is_within_the_display_limit_of_the_reference_export";
    let Some(adapter) = host_adapter(test) else {
        return;
    };
    let gpu = json!({"record": "gpu", "reason": null});
    for (name, photograph) in [
        ("generated", generated(1200, 800)),
        ("presence", presence_fixture()),
    ] {
        let exports = Exports::new(name, exporter(&adapter), &photograph);
        let original = exports.original();
        exports.edit(
            "edit.set-basic",
            json!({"exposure": 0.6, "contrast": 25, "saturation": 15}),
        );
        let (asked, reference) = exports.export("basic-reference.jpg", true);
        assert_eq!(
            asked["renderer"],
            json!({"record": "reference", "reason": "requested"})
        );
        let (drawn, file) = exports.export("basic-gpu.jpg", false);
        assert_eq!(drawn["renderer"], gpu, "{name}: {drawn}");
        assert_eq!(
            (drawn["width"].clone(), drawn["height"].clone()),
            (asked["width"].clone(), asked["height"].clone())
        );
        let statistics = decoded_against(&file, &reference);
        eprintln!("{test}: {name}, Basic, against the reference export: {statistics:?}");
        let verdict = luxforge_reference::preview_error::verdict(&statistics, Class::Pointwise);
        assert!(verdict.passed(), "{name}, Basic: {statistics:?}");

        exports.edit(
            "edit.set-presence",
            json!({"texture": 30, "clarity": 25, "dehaze": 20}),
        );
        let (drawn, file) = exports.export("presence-gpu.jpg", false);
        assert_eq!(drawn["renderer"], gpu, "{name}: {drawn}");
        let (_, reference) = exports.export("presence-reference.jpg", true);
        let statistics = decoded_against(&file, &reference);
        eprintln!("{test}: {name}, Presence, against the reference export: {statistics:?}");
        let verdict = luxforge_reference::preview_error::verdict(&statistics, Class::Spatial);
        assert!(verdict.passed(), "{name}, Presence: {statistics:?}");
        assert!(exports.original() == original, "{name}: the original");
    }
}

/// Two GPU exports of one stack through the export lane are the same bytes: twice through one
/// worker, and once through a second worker's own device on the same adapter, under another
/// owner, over the same photograph, Dehaze's light computed by each worker's runner.
#[test]
fn two_gpu_exports_are_byte_identical() {
    let test = "two_gpu_exports_are_byte_identical";
    let Some(adapter) = host_adapter(test) else {
        return;
    };
    let photograph = generated(1200, 800);
    let (first_worker, second_worker) = (exporter(&adapter), exporter(&adapter));
    let (one, two) = (
        Exports::new("identical-one", Arc::clone(&first_worker), &photograph),
        Exports::new("identical-two", Arc::clone(&second_worker), &photograph),
    );
    for exports in [&one, &two] {
        exports.edit("edit.set-basic", json!({"exposure": 0.4, "contrast": 20}));
        exports.edit(
            "edit.set-presence",
            json!({"texture": 30, "clarity": 25, "dehaze": 20}),
        );
    }
    let gpu = json!({"record": "gpu", "reason": null});
    let (first, file) = one.export("first.jpg", false);
    let (again, repeat) = one.export("again.jpg", false);
    let (other, elsewhere) = two.export("other.jpg", false);
    for result in [&first, &again, &other] {
        assert_eq!(result["renderer"], gpu, "{result}");
    }
    assert!(file == repeat, "twice through one device");
    assert!(file == elsewhere, "through a second device");
    // Each export's stream computes Presence's light once, before its tiles.
    assert_eq!(
        first_worker.figures().lights,
        2,
        "one light for each of two streams"
    );
    assert_eq!(second_worker.figures().lights, 1);
    eprintln!("{test}: {} bytes, the same three times", file.len());
}

/// A module query that reads pixels names the renderer that drew them, as `render.sample` and
/// `mask.sample-input` do: the neutral picker's patch and Auto tone's grid, each read by the GPU
/// tile worker the owner serves its reads through.
#[test]
fn pixel_reading_queries_name_the_gpu_that_drew_them() {
    let test = "pixel_reading_queries_name_the_gpu_that_drew_them";
    let Some(adapter) = host_adapter(test) else {
        return;
    };
    let exports = Exports::new("queries", exporter(&adapter), &generated(900, 600));
    // The generated photograph's flat grey patch, which the picker can neutralise.
    for (query, mut params) in [
        ("query.neutral-sample", json!({"x": 75, "y": 500})),
        ("query.auto-tone", json!({})),
    ] {
        params["asset_id"] = json!(exports.asset);
        let (answer, _) = owner_call(&exports.owner, exports.client, query, params)
            .unwrap_or_else(|error| panic!("{query}: {error}"));
        assert_eq!(
            answer["renderer"],
            json!({"record": "gpu", "reason": null}),
            "{query}: {answer}"
        );
    }
}

/// No export changes the original: GPU exports with and without metadata, the reference export,
/// and an export to a name already taken, which is refused and replaces nothing. The original's
/// folder holds nothing new but the exports.
#[test]
fn a_gpu_export_leaves_the_original_unchanged() {
    let test = "a_gpu_export_leaves_the_original_unchanged";
    let Some(adapter) = host_adapter(test) else {
        return;
    };
    let exports = Exports::new("original", exporter(&adapter), &generated(900, 600));
    let original = exports.original();
    exports.edit("edit.set-basic", json!({"exposure": -0.3, "contrast": 15}));
    let (stripped, file) = exports.export("stripped.jpg", false);
    assert_eq!(stripped["renderer"]["record"], "gpu", "{stripped}");
    let kept = json!({
        "asset_id": exports.asset, "destination": exports.dir.join("kept.jpg"),
        "keep_metadata": true, "mutation": {"request_id": "export-kept", "actor": "test"},
    });
    owner_call(&exports.owner, exports.client, "export.jpeg", kept).expect("an export");
    let (_, reference) = exports.export("reference.jpg", true);
    let refused = owner_call(
        &exports.owner,
        exports.client,
        "export.jpeg",
        json!({
            "asset_id": exports.asset, "destination": exports.dir.join("stripped.jpg"),
            "mutation": {"request_id": "export-taken", "actor": "test"},
        }),
    );
    assert!(refused.is_err(), "a name already taken is refused");
    luxforge_testbase::wait_until("the kept export", || exports.dir.join("kept.jpg").exists());
    assert!(exports.original() == original, "the original");
    assert!(std::fs::read(exports.dir.join("stripped.jpg")).unwrap() == file);
    assert!(file != reference, "the GPU's file and the reference's");
    let mut names: Vec<String> = std::fs::read_dir(&exports.dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".jpg"))
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["kept.jpg", "original.jpg", "reference.jpg", "stripped.jpg"]
    );
}

/// A launch that refused the GPU (`--no-gpu-render`) exports the reference's file, naming
/// `refused`, and one whose window has not named its adapter yet names `surface-pending`; neither
/// worker starts a thread or opens a device. Neither needs an adapter on this host.
#[test]
fn a_no_gpu_launch_exports_the_reference_naming_why() {
    let photograph = generated(240, 160);
    for (refused, reason) in [(true, "refused"), (false, "surface-pending")] {
        let worker = Arc::new(GpuTiles::pending(refused));
        let exports = Exports::new(reason, Arc::clone(&worker), &photograph);
        let original = exports.original();
        exports.edit("edit.set-basic", json!({"exposure": 0.5}));
        let (result, file) = exports.export("default.jpg", false);
        assert_eq!(
            result["renderer"],
            json!({"record": "reference", "reason": reason})
        );
        let (_, reference) = exports.export("reference.jpg", true);
        assert!(file == reference, "{reason}: the reference's own file");
        assert!(!worker.started(), "{reason}: nothing started");
        assert_eq!(worker.figures().adapter, None, "{reason}: nothing opened");
        assert!(exports.original() == original, "{reason}: the original");
    }
}

/// A worker the launch named, whose window then names another adapter while a stream has a tile
/// in flight: between jobs the worker lets go of its runner, the tile in flight unread, and the
/// stream ends at its next step naming `adapter-mismatch`, so the export lane draws it again with
/// the reference and no output mixes two adapters' tiles. The window naming the same adapter
/// confirms it, and a stream draws on to its end.
#[test]
fn the_windows_other_adapter_ends_a_stream_drawn_on_the_launchs() {
    use super::gpu_tiles::AdapterNaming;
    let test = "the_windows_other_adapter_ends_a_stream_drawn_on_the_launchs";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let (_, recipe) = families()
        .into_iter()
        .find(|(family, _)| *family == "Presence after Detail")
        .expect("the family");
    let client = clients(1)[0];
    for other in [false, true] {
        let service = GpuTiles::pending(false);
        assert!(service.adopt_adapter(&backend, &name, AdapterNaming::Launch));
        service.draw_streams_at(vec![SIDE]);
        let (stack, _) = fresh_stack(&stack(&source(BoundaryFormat::Half), &recipe));
        let begin = shut();
        service.hold_steps(Some(Arc::clone(&begin)));
        let mut bands = service.stream(&stack, &Cancel::new()).expect("a stream");
        drop(stack);
        wait_until("the worker held before the stream begins", || {
            begin.holding()
        });
        let first = next_step(&service, &begin);
        let second = next_step(&service, &first);
        assert_eq!(service.figures().in_flight, 1, "a tile in flight");
        let window = if other { "Another GPU" } else { name.as_str() };
        assert!(service.adopt_adapter(&backend, window, AdapterNaming::Window));
        service.hold_steps(None);
        second.open();
        if other {
            let ended = bands.next().expect("the end").expect_err("ended");
            assert_eq!(
                bands.fallback(),
                Some(&TileFallback::Unavailable(TileUnavailable::AdapterMismatch)),
                "{ended}"
            );
            assert!(bands.next().is_none());
            settle(&service, client);
            let after = service.figures();
            assert_eq!(after.in_flight, 0, "the tile in flight let go unread");
            assert_eq!(after.bands, 0, "no band of the old adapter's tiles sent");
        } else {
            let stitched: Vec<_> = bands.by_ref().map(|band| band.expect("a band")).collect();
            assert!(!stitched.is_empty() && bands.fallback().is_none());
            assert_eq!(service.figures().status, TileStatus::Gpu);
        }
        assert_eq!(service.figures().named, Some(AdapterNaming::Window));
    }
}

/// The runner's figures for exports of the measured stacks streamed staged and chained, each on a
/// worker of its own, as an indication and not a timing run: the 60 MP drag stack, which plans one
/// sweep and so streams chained either way, and the Air 2S's masked stack over the corpus RAW,
/// before its lens profile, which plans staged sweeps. The two streams of each are byte-identical.
/// `LUXFORGE_RAW_MANIFEST=MANIFEST cargo test --release -p luxforge-app
/// a_measured_export_staged_is_chained -- --ignored --nocapture`.
#[test]
#[ignore = "the corpus RAWs: set LUXFORGE_RAW_MANIFEST to the private RAW manifest"]
fn a_measured_export_staged_is_chained() {
    use super::gpu_qualification::{Opened, corpus_sources};
    use super::gpu_rest_tests::{committed, measured_stacks};
    let test = "a_measured_export_staged_is_chained";
    let Some(adapter) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let read = |path: std::path::PathBuf| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("readable")).expect("JSON")
    };
    let manifest = read(
        std::env::var("LUXFORGE_RAW_MANIFEST")
            .expect("a manifest")
            .into(),
    );
    let corpus = read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/preview/corpus.json"),
    );
    let found = corpus_sources(
        &corpus,
        std::path::Path::new("/nonexistent"),
        Some(&manifest),
    );
    let air = found
        .iter()
        .find(|source| source.id == "raw-air2s")
        .expect("the Air 2S");
    let dir = paths::temp_dir("gpu-tiles-measured");
    let opened = Opened::new(air, &[], &dir.join("air2s.sqlite")).expect("the RAW opened");
    let job = super::tasks::ready_preview_job(
        &opened.owner,
        luxforge_core::PreviewRequest::new(opened.client, opened.asset.clone()),
    )
    .expect("a job");
    let mut stacks = measured_stacks();
    let (_, _, masked) = stacks.swap_remove(1);
    let (_, sixty, drag) = stacks.swap_remove(0);
    let mut recipe = job.evaluation.recipe().clone();
    let geometry = recipe
        .layers
        .iter()
        .position(|layer| layer.effect_id.contains("lens"))
        .unwrap_or(recipe.layers.len());
    recipe.layers.splice(geometry..geometry, masked.layers);
    recipe.masks.extend(masked.masks);
    let measured = [
        ("the 60 MP drag stack", committed(sixty, drag)),
        (
            "the Air 2S masked stack",
            committed(job.evaluation.source().clone(), recipe),
        ),
    ];
    let budget = GPU_TILE_BUDGET - super::gpu_tiles::STREAM_READ_RESERVE;
    for (what, stack) in measured {
        let staging = luxforge_core::plan_stream_sweeps(&stack, budget)
            .unwrap_or_else(|fallback| panic!("{what}: {fallback:?}"));
        match staging.sweeps() {
            Some(sweeps) => eprintln!(
                "{test}: {what}: {} sweeps of {:?} tiles at {:?} px, {} stage textures of {:.1} MB",
                sweeps.sweeps.len(),
                sweeps
                    .sweeps
                    .iter()
                    .map(|sweep| sweep.tiles.len())
                    .collect::<Vec<_>>(),
                sweeps
                    .sweeps
                    .iter()
                    .map(|sweep| sweep.side)
                    .collect::<Vec<_>>(),
                sweeps.textures,
                sweeps.texture_bytes as f64 / 1e6
            ),
            None => eprintln!("{test}: {what}: chained, {staging:?}"),
        }
        let mut streamed = Vec::new();
        for chained in [false, true] {
            let service = GpuTiles::new(Some(adapter.clone()), false);
            service.chain_streams(chained);
            let client = clients(1)[0];
            let started = std::time::Instant::now();
            let mut rgba = Vec::new();
            for band in service
                .stream(&stack, &Cancel::new())
                .unwrap_or_else(|fallback| panic!("{what}: {fallback:?}"))
            {
                rgba.extend(
                    band.unwrap_or_else(|error| panic!("{what}: {error:?}"))
                        .rgba,
                );
            }
            let wall = started.elapsed();
            settle(&service, client);
            let figures = service.figures();
            let total = figures.total;
            eprintln!(
                "{test}: {what}, {}: {} tiles, {} streamed staged, {:.0} ms wall; light {:.0} ms, \
                 upload {:.0} ms, encode {:.0} ms, wait {:.0} ms, read {:.0} ms; peak {:.1} MB, \
                 {} uploads, {} slots, {} compiles, {} lights",
                if chained { "chained" } else { "as planned" },
                figures.tiles,
                figures.staged,
                wall.as_secs_f64() * 1e3,
                total.light_us as f64 / 1e3,
                total.upload_us as f64 / 1e3,
                total.encode_us as f64 / 1e3,
                total.wait_us as f64 / 1e3,
                total.read_us as f64 / 1e3,
                figures.peak as f64 / 1e6,
                figures.uploads,
                figures.slots,
                figures.compiles,
                figures.lights
            );
            assert_eq!(figures.stage_bytes, 0, "{what}: no stage held once it ends");
            streamed.push(rgba);
        }
        assert!(
            streamed[0] == streamed[1],
            "{what}: the streams as planned and chained differ"
        );
    }
}

/// A stack whose Dehaze reads the output of a spatial layer before it — the owner's masked Dehaze
/// behind Clarity, Dehaze behind Detail, and the first before a lens warp — streamed in staged
/// sweeps, the runner reducing the light from the stage texture the sweep before it wrote, is bit
/// for bit the photo surface's own staged picture at rest drawn at full size on another device of
/// the same adapter, whose sweep reduces the same light from its own stage texture, on both
/// paths. A read through such a light
/// draws its one tile on the GPU, reading the light the worker's staged sweeps of the stack
/// computed first and kept: the stream's bytes, its stage textures let go after. With no stage
/// texture, a worker drawing the stream chained after light sweeps, and reading through them, draws
/// the same bytes.
#[test]
fn a_staged_stream_reads_the_light_behind_a_spatial_layer_from_its_stage() {
    let test = "a_staged_stream_reads_the_light_behind_a_spatial_layer_from_its_stage";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    assert!(install_output_encoding());
    let window = adapters::open(&backend, &name)
        .unwrap_or_else(|unopened| panic!("{test}: the surface's device: {unopened:?}"));
    let mut surface = HeadlessSurface::new(&window.device, &window.queue);
    let service = GpuTiles::new(Some((backend.clone(), name.clone())), false);
    service.poison(true);
    service.draw_streams_at(vec![STAGED_SIDE]);
    let client = clients(1)[0];
    let budget = GPU_TILE_BUDGET - super::gpu_tiles::STREAM_READ_RESERVE;
    use super::gpu_tiles_tests::{radial, recipe};
    let families = [
        (
            "a masked Dehaze behind Clarity",
            recipe(
                vec![
                    Layer::new(PRESENCE_EFFECT, json!({"clarity": 60.0})),
                    Layer::new(PRESENCE_EFFECT, json!({"dehaze": 60.0})),
                ],
                vec![(1, radial())],
            ),
        ),
        (
            "Dehaze behind Detail",
            recipe(
                vec![
                    Layer::new(DETAIL_EFFECT, json!({"sharpening": 80.0, "radius": 1.0})),
                    Layer::new(PRESENCE_EFFECT, json!({"dehaze": -60.0})),
                ],
                Vec::new(),
            ),
        ),
        (
            "Dehaze behind Clarity before a lens warp",
            recipe(
                vec![
                    Layer::new(PRESENCE_EFFECT, json!({"clarity": 60.0})),
                    Layer::new(PRESENCE_EFFECT, json!({"dehaze": 60.0})),
                    luxforge_core::qualification::lens_layer(-0.06, STAGED),
                ],
                vec![(1, radial())],
            ),
        ),
    ];
    let bounds = luxforge_core::ProxyBounds {
        width: 400,
        height: 250,
    };
    let mut versions = 300;
    for (format, path) in [
        (BoundaryFormat::Half, "the byte path"),
        (BoundaryFormat::Float, "the linear path"),
    ] {
        let source = staged_source(format);
        versions += 1;
        let gpu = gpu_source(versions, &source);
        for (family, recipe) in &families {
            let what = format!("{family} on {path}");
            let stack = stack(&source, recipe);
            let staging = plan_stream_sweeps_at(&stack, budget, &[STAGED_SIDE])
                .unwrap_or_else(|fallback| panic!("{what}: {fallback:?}"));
            let Some(sweeps) = staging.sweeps() else {
                panic!("{what}: planned chained, {staging:?}");
            };
            assert!(
                sweeps.sweeps.iter().any(|sweep| sweep.lights == [0]),
                "{what}: a sweep reduces the light from its stage"
            );
            // The surface's staged picture at rest at full size.
            let tiles = luxforge_core::qualification::rest_tiles(&stack, bounds, STAGED_SIDE)
                .unwrap_or_else(|reason| panic!("{what}: {reason}"));
            versions += 1;
            let mut rest = super::gpu_preview::rest_now(&gpu, &tiles, versions).unwrap();
            rest.reduction = Some(super::gpu_rest_tests::at_full_size(&tiles));
            let drawn = surface
                .rest(&gpu, &rest)
                .unwrap_or_else(|fallback| panic!("{what}: the surface: {fallback:?}"));
            assert!(drawn.figures.sweeps >= 2, "{what}: drawn in sweeps");
            settle(&service, client);
            let bands = service
                .stream(&stack, &Cancel::new())
                .unwrap_or_else(|fallback| panic!("{what}: {fallback:?}"));
            assert_eq!(bands.answered(), &Answered::gpu(), "{what}");
            let rgba = stitched(bands, STAGED_SIDE);
            let width = tiles.output.width;
            assert_eq!(rgba.len(), drawn.codes.len() * 4, "{what}: the whole stage");
            if let Some(difference) = first_difference(width, &rgba, &drawn.codes.concat()) {
                panic!(
                    "{what}: the staged stream against the surface's picture at rest: {difference}"
                );
            }
            settle(&service, client);
            let figures = service.figures();
            assert_eq!(
                (figures.stage_bytes, figures.in_use, figures.in_flight),
                (0, 0, 0),
                "{what}: nothing held once it ends"
            );
            // A read through the light draws one tile, which reads the light the worker's staged
            // sweeps of the stack computed and kept: the stream's codes at every point.
            let points: Vec<Read> = [(17, 23), (800, 500), (1599, 999), (1201, 77)]
                .into_iter()
                .map(|(x, y)| point(ReadStage::Output, x, y, ReadValues::Codes))
                .collect();
            let (answers, _) = call(&service, client, &stack, &points);
            for (answer, (_, rect, _)) in answers.iter().zip(&points) {
                assert_eq!(answer.answered, Answered::gpu(), "{what}: {rect:?}");
                let ReadPixels::Codes(codes) = &answer.pixels else {
                    panic!("{what}: codes");
                };
                let at = ((rect.y0 * width + rect.x0) * 4) as usize;
                assert_eq!(
                    codes[0][..3],
                    rgba[at..at + 3],
                    "{what}: the read at {rect:?} is the stream's byte"
                );
            }
            settle(&service, client);
            assert_eq!(
                service.figures().stage_bytes,
                0,
                "{what}: the read's sweeps let their stage textures go"
            );
            // With no stage texture: a worker of its own, which keeps no light, draws the stream
            // chained after a light sweep that reduces each tile's rectangle into the light, and
            // its reads likewise: the staged stream's bytes, the light the same bits.
            let stage_free = GpuTiles::new(Some((backend.clone(), name.clone())), false);
            stage_free.poison(true);
            stage_free.draw_streams_at(vec![STAGED_SIDE]);
            stage_free.chain_streams(true);
            let (answers, _) = call(&stage_free, client, &stack, &points);
            for (answer, (_, rect, _)) in answers.iter().zip(&points) {
                assert_eq!(
                    answer.answered,
                    Answered::gpu(),
                    "{what}: stage-free {rect:?}"
                );
                let ReadPixels::Codes(codes) = &answer.pixels else {
                    panic!("{what}: codes");
                };
                let at = ((rect.y0 * width + rect.x0) * 4) as usize;
                assert_eq!(
                    codes[0][..3],
                    rgba[at..at + 3],
                    "{what}: stage-free {rect:?}"
                );
            }
            let bands = stage_free
                .stream(&stack, &Cancel::new())
                .unwrap_or_else(|fallback| panic!("{what}: stage-free: {fallback:?}"));
            assert_eq!(bands.answered(), &Answered::gpu(), "{what}: stage-free");
            let free = stitched(bands, STAGED_SIDE);
            if let Some(difference) = first_difference(width, &free, &rgba) {
                panic!("{what}: the stage-free stream against the staged one: {difference}");
            }
            settle(&stage_free, client);
            let figures = stage_free.figures();
            assert_eq!(
                (
                    figures.stage_bytes,
                    figures.in_use,
                    figures.in_flight,
                    figures.staged
                ),
                (0, 0, 0, 0),
                "{what}: no stage texture, nothing held once it ends"
            );
            eprintln!(
                "{test}: {what}: {} sweeps, bit for bit the surface's staged picture at rest, and so with no stage texture",
                sweeps.sweeps.len()
            );
        }
    }
}

/// Integrated native before/after stream measurement. Sources/stacks match the existing ledger;
/// the consumer drops each band after counting it, so no full output buffer or JPEG encoding is timed.
#[test]
#[ignore = "integrated native export-stream timing on a quiet host, 30 samples per workload"]
fn code_structure_export_measurement() {
    use super::gpu_rest_tests::{committed, measured_stacks};
    use luxforge_testbase::Distribution;
    let adapter = host_adapter("code_structure_export_measurement").expect("a native adapter");
    assert!(install_output_encoding());
    let mut stacks = measured_stacks();
    // Keep the representative 60 MP heavy JPEG, 20 MP developed RAW masked stack and 24 MP JPEG.
    stacks.remove(2);
    for (name, source, recipe) in stacks {
        let dimensions = source.dimensions();
        let stack = committed(source, recipe);
        let service = GpuTiles::new(Some(adapter.clone()), false);
        let client = clients(1)[0];
        let mut wall = Vec::new();
        let mut compiles = Vec::new();
        let mut peak = Vec::new();
        for sample in 0..31 {
            settle(&service, client);
            let before = service.figures();
            let started = std::time::Instant::now();
            let mut bytes = 0usize;
            for band in service
                .stream(&stack, &Cancel::new())
                .expect("a GPU stream")
            {
                bytes += band.expect("a complete band").rgba.len();
            }
            let ms = started.elapsed().as_secs_f64() * 1e3;
            assert_eq!(bytes, dimensions.0 as usize * dimensions.1 as usize * 4);
            settle(&service, client);
            let after = service.figures();
            assert_eq!(after.stage_bytes, 0, "no stage held after completion");
            if sample != 0 {
                wall.push(ms);
                compiles.push(after.compiles - before.compiles);
                peak.push(after.peak);
            }
        }
        let dist = Distribution::of(wall).unwrap();
        println!(
            "STRUCTURE_EXPORT {}",
            json!({
                "workload":name,"dimensions":dimensions,"adapter":adapter,"samples":dist.count,
                "wall_ms":{"p50":dist.p50,"p95":dist.p95,"all":dist.samples},
                "gpu_compiles":compiles,"runner_charged_peak_bytes":peak,
                "scope":"production GPU export stream, warmed worker/source/shader caches after one discarded warmup; band consumption; excludes source development/JPEG encoding and GPU resources outside current charges"
            })
        );
    }
}

#[test]
fn auto_tone_gpu_grid_and_values_agree_with_the_reference() {
    let Some((backend, name)) =
        host_adapter("auto_tone_gpu_grid_and_values_agree_with_the_reference")
    else {
        return;
    };
    assert!(install_output_encoding());
    let gpu = GpuTiles::new(Some((backend, name)), false);
    let reference = ReferenceTiles::new();
    let client = clients(1)[0];
    for format in [BoundaryFormat::Half, BoundaryFormat::Float] {
        let source = source(format);
        for (prefix, layers) in [
            (0, vec![Layer::new(BASIC_EFFECT, json!({}))]),
            (
                1,
                vec![
                    Layer::new(DETAIL_EFFECT, json!({"sharpening": 60., "luminance": 30.})),
                    Layer::new(BASIC_EFFECT, json!({})),
                    luxforge_core::qualification::lens_layer(-0.06, (WIDTH, HEIGHT)),
                    Layer::new(
                        luxforge_core::PERSPECTIVE_EFFECT,
                        json!({"horizontal":25,"vertical":-15}),
                    ),
                    Layer::new(
                        luxforge_core::CROP_EFFECT,
                        json!({"x":0.1,"y":0.1,"width":0.8,"height":0.8,"angle":3.}),
                    ),
                ],
            ),
        ] {
            let recipe = super::gpu_tiles_tests::recipe(layers, vec![]);
            let evaluation = stack(&source, &recipe);
            let analyse = |service: &dyn TileService| {
                let held = evaluation.clone();
                let (sender, receiver) = mpsc::sync_channel(1);
                service.submit(TileCall::caller(client, Cancel::new(), move |reads, cancel| {
                let sampled = luxforge_core::tiles::read_grid(&held, prefix, reads, cancel)?;
                let model = luxforge_core::auto_tone::forward_model(held.registry(), &held.recipe().layers, prefix, luxforge_core::CompileStage::exact(luxforge_core::Stage { width: 32, height: 32 }))?;
                let report = model.solve(&sampled.sample, Default::default(), cancel)?;
                Ok(json!({"renderer": sampled.answered.record, "rgb": sampled.sample.rgb, "values": report.values}))
            }, move |answer| { let _ = sender.send(answer); }));
                receiver.recv_timeout(HANG).unwrap().unwrap()
            };
            let gpu_result = analyse(&gpu);
            let reference_result = analyse(&reference);
            assert_eq!(gpu_result["renderer"], "gpu", "{gpu_result}");
            let pixels = |value: &Value| {
                serde_json::from_value::<Vec<[f32; 3]>>(value["rgb"].clone()).unwrap()
            };
            let ours = pixels(&gpu_result);
            let theirs = pixels(&reference_result);
            assert_eq!(ours.len(), theirs.len());
            let mut delta_e = Vec::new();
            let mut delta_l = Vec::new();
            for (ours, theirs) in ours.iter().zip(&theirs) {
                let ours = lab_from_srgb8(ours.map(code));
                let theirs = lab_from_srgb8(theirs.map(code));
                delta_e.push(ciede2000(ours, theirs));
                delta_l.push(ours[0] - theirs[0]);
            }
            let errors = statistics_of(ours.len(), 1, &delta_e, &delta_l).unwrap();
            let limits = if prefix == 0 {
                Class::Pointwise
            } else {
                Class::Spatial
            }
            .limits();
            assert!(
                errors.mean <= limits.mean
                    && errors.max <= limits.worst_block // Also bounds every possible 16 × 16 mean, including a clipped grid.
                    && errors.p99 <= limits.p99
                    && errors.mean_delta_l.abs() <= limits.mean_delta_l
            );
            eprintln!("Auto grid {format:?}, prefix {prefix}: {errors:?}");
            for field in luxforge_core::auto_tone::FIELDS {
                let difference = (gpu_result["values"][field].as_f64().unwrap()
                    - reference_result["values"][field].as_f64().unwrap())
                .abs();
                assert!(
                    difference <= if field == "exposure" { 0.0200001 } else { 2. },
                    "{field}: {difference}"
                );
            }
        }
    }
}
