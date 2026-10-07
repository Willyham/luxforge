//! The picture at rest drawn in tiles (`docs/design/gpu-preview.md`, "The picture at rest"), on a
//! headless device the test creates, read back through the photograph's real draw: the tiles drawn
//! a frame at a time, each over its own window of the source, reduced into the view's size as the
//! reference's area average reduces the frame they make, and drawn in place of the photograph once
//! the last is in.
use super::super::headless::HeadlessSurface;
use super::super::histogram::Counts;
use super::*;
use crate::photo_surface::{
    CountsOutcome, GpuRegion, GpuRest, GpuSource, RestFigures, RestReduction,
};
use luxforge_reference::tolerance;

/// Codes that vary along both axes, `width` × `height` pixels of four bytes each.
fn codes(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend([
                ((x * 5 + y * 3) % 256) as u8,
                ((x * 2 + y * 9 + 7) % 256) as u8,
                ((x ^ (2 * y)) % 256) as u8,
                255,
            ]);
        }
    }
    rgba
}

/// The area average of an axis of `from` pixels into `to`, as the reference reduces it: for each
/// output index the first source index, its weights and the weights in `f64`.
fn coverage(from: u32, to: u32) -> (Vec<u32>, Vec<u32>, Vec<f64>) {
    let (mut first, mut offsets, mut weights) = (Vec::new(), vec![0], Vec::new());
    let scale = f64::from(from) / f64::from(to);
    for index in 0..to {
        let (start, end) = (f64::from(index) * scale, f64::from(index + 1) * scale);
        let lowest = start.floor() as u32;
        first.push(lowest);
        let mut at = lowest;
        while f64::from(at) < end && at < from {
            let covered = end.min(f64::from(at + 1)) - start.max(f64::from(at));
            weights.push(covered / scale);
            at += 1;
        }
        offsets.push(weights.len() as u32);
    }
    (first, offsets, weights)
}

fn axis(
    (first, offsets, weights): &(Vec<u32>, Vec<u32>, Vec<f64>),
) -> crate::photo_surface::AxisCoverage {
    crate::photo_surface::AxisCoverage {
        first: first.clone(),
        offsets: offsets.clone(),
        weights: weights.iter().map(|weight| *weight as f32).collect(),
    }
}

/// A picture at rest over a 128 × 96 output stage in tiles of 48, reduced to the 64 × 64 view the
/// surface draws: drawn a tile a frame, the CPU frame drawn until the last is in, then the rest
/// output in place of the photograph, each view pixel the reference's area average of the tiles'
/// codes within a code, and the same bytes when drawn again. Another version starts over.
#[test]
fn a_picture_at_rest_is_drawn_in_tiles_and_reduced_to_the_view() {
    let test = "a_picture_at_rest_is_drawn_in_tiles_and_reduced_to_the_view";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    if let Err(error) = super::super::rest::RestPasses::new(&device) {
        panic!("the rest passes: {error}");
    }
    let (width, height) = (WIDTH, HEIGHT);
    let rgba = codes(width, height);
    let source = GpuSource::codes(3, Arc::new(rgba.clone()), width, height).expect("a source");
    let tiles = tiles_of(&source);
    assert_eq!(tiles.len(), 6);
    let (across, down) = (coverage(width, SIDE), coverage(height, SIDE));
    let rest = rest_of(&tiles);
    pipeline.compile_now(&device, &tiles[0]);
    let primitive = PhotoPrimitive {
        source: Some(source.clone()),
        rest: Some(rest.clone()),
        ..primitive(ID, None)
    };
    let held = held_codes(&rgba);
    let mut frames = 0;
    let drawn = loop {
        let drawn = paint(&device, &queue, &mut pipeline, &primitive);
        frames += 1;
        let seen = diagnostics(&pipeline, ID);
        let figures = seen.gpu_rest.expect("the rest's figures");
        assert_eq!(figures.fallback, None, "frame {frames}: {figures:?}");
        if seen.drawn_rest == Some(1) {
            assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
            assert!(figures.done);
            break drawn;
        }
        assert_cpu_frame(&drawn);
        assert!(figures.drawn <= 6 && !figures.done, "{figures:?}");
        assert!(frames < 6, "the rest was never drawn: {figures:?}");
    };
    assert_eq!(
        frames, 6,
        "one tile a frame, drawn in the frame that draws the last"
    );
    let mut exact = 0;
    for y in 0..SIDE as usize {
        for x in 0..SIDE as usize {
            let reference: [u8; 3] = [0, 1, 2].map(|channel| {
                let mut sum = 0.0;
                for j in down.1[y]..down.1[y + 1] {
                    let sy = (down.0[y] + j - down.1[y]) as usize;
                    for i in across.1[x]..across.1[x + 1] {
                        let sx = (across.0[x] + i - across.1[x]) as usize;
                        sum += down.2[j as usize]
                            * across.2[i as usize]
                            * srgb::decode(held[sy * width as usize + sx][channel]);
                    }
                }
                srgb::code(sum)
            });
            let at = (y * SIDE as usize + x) * 4;
            let gpu = [drawn[at + 2], drawn[at + 1], drawn[at]];
            for channel in 0..3 {
                assert!(
                    gpu[channel].abs_diff(reference[channel]) <= 1,
                    "({x}, {y}) channel {channel}: {} against {}",
                    gpu[channel],
                    reference[channel]
                );
            }
            exact += usize::from(gpu == reference);
        }
    }
    eprintln!(
        "{test}: {exact} of {} view pixels equal to the f64 reference's codes",
        SIDE * SIDE
    );
    // Its tiles' counts are the whole stage's, every pixel once, exactly the reference's.
    let counts = rest_counts(&device, &pipeline, 1);
    assert_eq!(counts.pixels, u64::from(width * height));
    assert_eq!(
        compared(&counts),
        bins_and_counters(&reference_counts(&held))
    );
    // Drawn again, the same bytes, and no tile drawn again.
    let again = paint(&device, &queue, &mut pipeline, &primitive);
    assert_eq!(again, drawn);
    // What its tiles did: every tile's links ran over its own window, the slot refitted wherever
    // a tile's window or rectangle is another shape than the one before, nothing waited for a
    // retirement, and once the GPU is done every tile's span is reported.
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .expect("the device finishes");
    paint(&device, &queue, &mut pipeline, &primitive);
    let figures = diagnostics(&pipeline, ID)
        .gpu_rest
        .expect("the rest's figures");
    let shape = |plan: &GpuPlan| {
        (
            plan.boundary.size(),
            plan.region.map(|region| region.size()),
        )
    };
    let refits = tiles
        .windows(2)
        .filter(|pair| shape(&pair[0]) != shape(&pair[1]))
        .count() as u32;
    let windows: u64 = tiles
        .iter()
        .map(|plan| {
            let (width, height) = plan.boundary.size();
            u64::from(width) * u64::from(height)
        })
        .sum();
    eprintln!("{test}: {figures:?}");
    let evaluation = figures.evaluation;
    assert_eq!(evaluation.refits, refits, "a refit at each change of shape");
    assert_eq!(
        evaluation.window_texels, windows,
        "every tile's window once"
    );
    assert!(evaluation.links_run >= 6, "every tile's link ran");
    assert_eq!(
        (evaluation.lights_encoded, evaluation.lights_restored),
        (0, 0)
    );
    assert_eq!(figures.retirement_waits, 0);
    assert_eq!(figures.gpu_tiles, 6, "every tile's span reported");
    assert!(figures.gpu_max_us > 0 && figures.gpu_max_us <= figures.gpu_us);
    // Another version starts over from its first tile, the CPU frame drawn meanwhile.
    let primitive = PhotoPrimitive {
        rest: Some(GpuRest { version: 2, ..rest }),
        ..primitive
    };
    assert_cpu_frame(&paint(&device, &queue, &mut pipeline, &primitive));
    let figures = diagnostics(&pipeline, ID)
        .gpu_rest
        .expect("the rest's figures");
    assert_eq!(
        figures,
        RestFigures {
            version: 2,
            tiles: 6,
            drawn: 1,
            done: false,
            waiting: false,
            dissolving: false,
            prepare_us: figures.prepare_us,
            fallback: None,
            counts_only: false,
            sweeps: 0,
            evaluation: figures.evaluation,
            retirement_waits: 0,
            gpu_tiles: figures.gpu_tiles,
            gpu_us: figures.gpu_us,
            gpu_max_us: figures.gpu_max_us,
        }
    );
    assert_eq!(figures.evaluation.window_texels, {
        let (width, height) = tiles[0].boundary.size();
        u64::from(width) * u64::from(height)
    });
    assert!(figures.prepare_us > 0, "its first tile's prepare timed");
    // None lets it go.
    paint(&device, &queue, &mut pipeline, &handing_none());
    assert_eq!(diagnostics(&pipeline, ID).gpu_rest, None);
    settle(&pipeline);
}

/// Surface [`ID`]'s photograph with no plan and no picture at rest.
fn handing_none() -> PhotoPrimitive {
    primitive(ID, None)
}

/// The output stage the pictures at rest here cover, and their tiles' side.
const WIDTH: u32 = 128;
const HEIGHT: u32 = 96;
const TILE: u32 = 48;

/// The identity's tiles of `source`'s 128 × 96 stage, 48 a side, row by row: each a region plan of
/// the stage over its own cut of the source.
fn tiles_of(source: &GpuSource) -> Vec<GpuPlan> {
    let mut tiles = Vec::new();
    for y0 in (0..HEIGHT).step_by(TILE as usize) {
        for x0 in (0..WIDTH).step_by(TILE as usize) {
            let (x1, y1) = ((x0 + TILE).min(WIDTH), (y0 + TILE).min(HEIGHT));
            let boundary = GpuBoundary::derived(
                source,
                Derivation::Cut { origin: (x0, y0) },
                x1 - x0,
                y1 - y0,
                10 + tiles.len() as u64,
            )
            .expect("a tile's boundary");
            tiles.push(GpuPlan {
                boundary,
                texels: TexelMap {
                    origin: [x0 as f32, y0 as f32],
                    step: [1.0, 1.0],
                },
                steps: vec![GpuStep::colour(identity())],
                region: Some(GpuRegion {
                    rect: [x0, y0, x1, y1],
                    stage: (WIDTH, HEIGHT),
                    full_stage: (WIDTH, HEIGHT),
                }),
                lights: Vec::new(),
            });
        }
    }
    tiles
}

/// The picture at rest of `tiles`, reduced to the 64 × 64 view.
fn rest_of(tiles: &[GpuPlan]) -> GpuRest {
    GpuRest {
        version: 1,
        tiles: tiles.to_vec().into(),
        reduction: Some(RestReduction {
            view: (SIDE, SIDE),
            across: axis(&coverage(WIDTH, SIDE)),
            down: axis(&coverage(HEIGHT, SIDE)),
        }),
        stages: None,
        light_sweeps: Arc::from([]),
    }
}

/// The independent reference's counts of `codes`, three codes a pixel
/// (`luxforge_reference::tolerance`), the reduction the release gate holds the GPU's to.
fn reference_counts(codes: &[[u8; 3]]) -> tolerance::Counts {
    let rgba: Vec<u8> = codes
        .iter()
        .flat_map(|[r, g, b]| [*r, *g, *b, 255])
        .collect();
    tolerance::Counts::of(&rgba, 4)
}

/// The bins and counters of `counts`, the GPU's, which carry no luminance histogram.
type Compared = ([[u64; 256]; 3], [u64; 11]);

/// The bins and counters of the independent comparison's counts.
fn bins_and_counters(counts: &tolerance::Counts) -> Compared {
    (counts.bins, counts.clipping)
}

/// `counts` as the independent comparison holds them: their bins and counters.
fn compared(counts: &Counts) -> Compared {
    (
        [counts.r, counts.g, counts.b],
        [
            counts.r0,
            counts.g0,
            counts.b0,
            counts.r255,
            counts.g255,
            counts.b255,
            counts.any_shadow,
            counts.any_highlight,
            counts.all_shadow,
            counts.all_highlight,
            counts.both,
        ],
    )
}

/// The counts `pipeline`'s surface [`ID`] holds of its picture at rest of `version`, once read
/// back, the device polled meanwhile.
fn rest_counts(device: &wgpu::Device, pipeline: &PhotoPipeline, version: u64) -> Counts {
    luxforge_testbase::wait_for("the picture at rest's counts", || {
        let _ = device.poll(wgpu::PollType::Poll);
        let held = pipeline.figures.counts.lock().unwrap().get(&ID).cloned()?;
        let (counted, counts) = held.rest?;
        assert_eq!(counted, version);
        match counts.outcome() {
            CountsOutcome::Counting => None,
            CountsOutcome::Ready(counts) => Some(*counts),
            CountsOutcome::Failed(error) => panic!("no counts: {error}"),
        }
    })
}

/// The identity's codes of `rgba`: each pixel's code decoded and held as the nearest half float,
/// encoded.
fn held_codes(rgba: &[u8]) -> Vec<[u8; 3]> {
    rgba.chunks_exact(4)
        .map(|pixel| {
            [0, 1, 2].map(|channel| {
                srgb::code(f64::from(
                    half::f16::from_f32(srgb::decode(pixel[channel]) as f32).to_f32(),
                ))
            })
        })
        .collect()
}

/// A picture at rest handed beside a plan drawn in place of the photograph — the desktop's view
/// plan at rest, or a released gesture's last plan — draws its tiles all the same, a tile a frame,
/// while the plan's output is drawn; the frame its last tile comes in starts its dissolve over that
/// output, each frame of which draws the output and then the rest output at the share, and once the
/// dissolve has run its course the rest output alone is the photograph.
#[test]
fn a_picture_at_rest_dissolves_in_over_the_plan_drawn_before_it() {
    let test = "a_picture_at_rest_dissolves_in_over_the_plan_drawn_before_it";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    if let Err(error) = super::super::rest::RestPasses::new(&device) {
        panic!("the rest passes: {error}");
    }
    let mut pipeline = own_pipeline(&device, &queue);
    let rgba = codes(WIDTH, HEIGHT);
    let source = GpuSource::codes(3, Arc::new(rgba), WIDTH, HEIGHT).expect("a source");
    let tiles = tiles_of(&source);
    let rest = rest_of(&tiles);
    let whole = GpuBoundary::derived(
        &source,
        Derivation::Cut { origin: (0, 0) },
        WIDTH,
        HEIGHT,
        7,
    )
    .expect("a boundary");
    let view = plan(&whole, vec![identity()]);
    pipeline.compile_now(&device, &tiles[0]);
    pipeline.compile_now(&device, &view);
    let primitive = PhotoPrimitive {
        source: Some(source.clone()),
        rest: Some(rest.clone()),
        ..primitive(ID, Some(view))
    };
    let mut frames = 0;
    let dissolve = loop {
        paint(&device, &queue, &mut pipeline, &primitive);
        frames += 1;
        let seen = diagnostics(&pipeline, ID);
        let figures = seen.gpu_rest.expect("the rest's figures");
        assert_eq!(figures.fallback, None, "frame {frames}");
        if let Some(dissolve) = seen.drawn_rest_dissolve {
            assert_eq!(seen.drawn_rest, Some(1), "the rest over the plan's output");
            assert_eq!(seen.drawn_gpu_boundary, Some(7));
            assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
            assert!(figures.done && figures.dissolving);
            break dissolve;
        }
        assert_eq!(
            seen.drawn_rest, None,
            "frame {frames}: not before its last tile"
        );
        assert_eq!(
            seen.drawn_gpu_boundary,
            Some(7),
            "frame {frames}: the plan's output"
        );
        assert!(frames < 6, "the rest never dissolved in: {figures:?}");
    };
    assert_eq!(frames, 6, "a tile a frame while the plan is drawn");
    assert_eq!((dissolve.from, dissolve.to), (7, 1));
    assert!(dissolve.share < DrawnDissolve::WHOLE);
    // A frame a look, through the one hang-bounded wait, until the dissolve has run its course.
    luxforge_testbase::wait_until("the dissolve's course", || {
        paint(&device, &queue, &mut pipeline, &primitive);
        diagnostics(&pipeline, ID).drawn_rest_dissolve.is_none()
    });
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_rest, Some(1));
    assert_eq!(seen.drawn_rest_dissolve, None, "its course run");
    assert_eq!(seen.drawn_gpu_boundary, None, "the rest output alone");
    assert!(seen.gpu_rest.is_some_and(|figures| !figures.dissolving));
    settle(&pipeline);
}

/// A picture at rest drawn headless ([`HeadlessSurface`]) is the one the surface draws — the codes
/// the photograph's draw shows, a tile a frame — and a tile drawn from a window of the source
/// holding only that tile's own window is the tile drawn from the whole source, bit for bit, each
/// pixel the identity's code of its own.
#[test]
fn a_picture_at_rest_drawn_headless_is_the_one_the_surface_draws() {
    let test = "a_picture_at_rest_drawn_headless_is_the_one_the_surface_draws";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    if let Err(error) = super::super::rest::RestPasses::new(&device) {
        panic!("the rest passes: {error}");
    }
    let mut pipeline = own_pipeline(&device, &queue);
    let rgba = codes(WIDTH, HEIGHT);
    let source = GpuSource::codes(3, Arc::new(rgba.clone()), WIDTH, HEIGHT).expect("a source");
    let tiles = tiles_of(&source);
    let rest = rest_of(&tiles);
    pipeline.compile_now(&device, &tiles[0]);
    let primitive = PhotoPrimitive {
        source: Some(source.clone()),
        rest: Some(rest.clone()),
        ..primitive(ID, None)
    };
    let drawn = loop {
        let drawn = paint(&device, &queue, &mut pipeline, &primitive);
        if diagnostics(&pipeline, ID).drawn_rest == Some(1) {
            break drawn;
        }
    };
    let mut surface = HeadlessSurface::new(&device, &queue);
    let headless = surface
        .rest(&source, &rest)
        .expect("the rest drawn headless");
    assert!(
        headless.frames >= 6,
        "a tile a frame at most, beside the frames that waited for the compile"
    );
    assert_eq!(headless.codes.len(), (SIDE * SIDE) as usize);
    for (index, code) in headless.codes.iter().enumerate() {
        let at = index * 4;
        assert_eq!(
            [code[0], code[1], code[2], code[3]],
            [drawn[at + 2], drawn[at + 1], drawn[at], 255],
            "view pixel {index}"
        );
    }
    let held = held_codes(&rgba);
    for plan in &tiles {
        let [x0, y0, x1, y1] = plan.region.expect("a region plan").rect;
        let whole = surface
            .tile(&source, plan)
            .expect("a tile of the whole source");
        let window = source
            .window([x0, y0, x1 - x0, y1 - y0])
            .expect("the tile's window");
        assert_eq!(window.bytes(), u64::from((x1 - x0) * (y1 - y0) * 4));
        let windowed = surface.tile(&window, plan).expect("a tile of its window");
        assert_eq!(windowed, whole, "the tile at ({x0}, {y0})");
        for (index, code) in whole.iter().enumerate() {
            let (x, y) = (x0 + index as u32 % (x1 - x0), y0 + index as u32 / (x1 - x0));
            assert_eq!(
                [code[0], code[1], code[2]],
                held[(y * WIDTH + x) as usize],
                "({x}, {y})"
            );
        }
    }
    settle(&pipeline);
}

/// A picture at rest with no reduction is drawn for its counts alone, a tile a frame: the CPU
/// frame stays the photograph, no rest output is made or drawn, and once its last tile is in its
/// counts are the whole stage's, exactly the reference's; drawn headless, the same counts.
#[test]
fn tiles_drawn_for_their_counts_alone_count_the_stage_and_draw_nothing() {
    let test = "tiles_drawn_for_their_counts_alone_count_the_stage_and_draw_nothing";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let rgba = codes(WIDTH, HEIGHT);
    let source = GpuSource::codes(3, Arc::new(rgba.clone()), WIDTH, HEIGHT).expect("a source");
    let tiles = tiles_of(&source);
    let rest = GpuRest {
        reduction: None,
        ..rest_of(&tiles)
    };
    pipeline.compile_now(&device, &tiles[0]);
    let primitive = PhotoPrimitive {
        source: Some(source.clone()),
        rest: Some(rest.clone()),
        ..primitive(ID, None)
    };
    let mut frames = 0;
    loop {
        assert_cpu_frame(&paint(&device, &queue, &mut pipeline, &primitive));
        frames += 1;
        let seen = diagnostics(&pipeline, ID);
        let figures = seen.gpu_rest.expect("the rest's figures");
        assert_eq!(figures.fallback, None, "frame {frames}");
        assert!(figures.counts_only);
        assert_eq!(seen.drawn_rest, None, "frame {frames}: nothing drawn");
        if figures.done {
            break;
        }
        assert!(frames < 6, "the tiles were never all drawn: {figures:?}");
    }
    assert_eq!(frames, 6, "one tile a frame");
    let expected = bins_and_counters(&reference_counts(&held_codes(&rgba)));
    let counts = rest_counts(&device, &pipeline, 1);
    assert_eq!(compared(&counts), expected);
    let mut surface = HeadlessSurface::new(&device, &queue);
    let drawn = surface.rest(&source, &rest).expect("drawn headless");
    assert!(drawn.codes.is_empty(), "no picture");
    assert_eq!(
        compared(&drawn.counts.expect("the counts headless")),
        expected
    );
    paint(&device, &queue, &mut pipeline, &handing_none());
    settle(&pipeline);
}

/// A gesture's tick is counted once, from the frame it drew, the frame on screen in motion, and
/// read back tagged with its boundary and revision; a plan drawn with the clipping overlay's marks
/// over its output, or with no tag, is not counted.
#[test]
fn a_gestures_tick_is_counted_once_from_the_frame_it_drew() {
    let test = "a_gestures_tick_is_counted_once_from_the_frame_it_drew";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, codes) = boundary_with_codes(9);
    let tick = |tag: Option<u64>, steps: Vec<GpuStep>| {
        let mut primitive = primitive(
            ID,
            Some(GpuPlan {
                steps,
                ..plan(&boundary, Vec::new())
            }),
        );
        primitive.gpu_options.tag = tag;
        primitive
    };
    let held = |pipeline: &PhotoPipeline| {
        pipeline
            .figures
            .counts
            .lock()
            .unwrap()
            .get(&ID)
            .and_then(|held| held.tick.clone())
    };
    // No tag: the view plan at rest, which a picture at rest's tiles count instead.
    paint(
        &device,
        &queue,
        &mut pipeline,
        &tick(None, vec![GpuStep::colour(identity())]),
    );
    assert!(held(&pipeline).is_none());
    // The clipping overlay's marks over the output: not output codes.
    let marks = GpuStep::Clipping(ClipMarks {
        shadows: true,
        highlights: true,
        shadow_below: 0.0,
        highlight_from: 1.0,
        palette: [[0, 0, 255, 255], [255, 0, 0, 255], [255, 0, 255, 255]],
    });
    paint(
        &device,
        &queue,
        &mut pipeline,
        &tick(Some(3), vec![GpuStep::colour(identity()), marks]),
    );
    assert!(held(&pipeline).is_none());
    // A tick: counted from its frame, exactly the reference's counts of the codes it drew.
    let drawn = tick(Some(4), vec![GpuStep::colour(identity())]);
    assert_codes(&paint(&device, &queue, &mut pipeline, &drawn), &codes);
    let counts = luxforge_testbase::wait_for("the tick's counts", || {
        let _ = device.poll(wgpu::PollType::Poll);
        let tick = held(&pipeline)?;
        assert_eq!(
            (tick.boundary, tick.revision),
            (9, 4),
            "the boundary and the revision"
        );
        assert_eq!(tick.size, (SIDE, SIDE), "the frame drawn");
        match tick.counts.outcome() {
            CountsOutcome::Counting => None,
            CountsOutcome::Ready(counts) => Some(*counts),
            CountsOutcome::Failed(error) => panic!("no counts: {error}"),
        }
    });
    assert_eq!(
        compared(&counts),
        bins_and_counters(&reference_counts(&codes))
    );
    // Drawn again, the same tick is not counted again.
    paint(&device, &queue, &mut pipeline, &drawn);
    assert!(matches!(
        held(&pipeline).map(|tick| tick.counts),
        Some(RestCounts::Reading(_))
    ));
    paint(&device, &queue, &mut pipeline, &handing_none());
    settle(&pipeline);
}
