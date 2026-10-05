//! The tile runner on a device of its own, on this host's adapter (`docs/design/gpu-preview.md`,
//! "Qualifying a program"): the window of the source it holds cuts what the whole source cuts, a
//! run past its budget creates nothing, a lost device answers `device-lost`, and an adapter it was
//! not asked for is never opened in the place of one it was. Its bit-for-bit identity with the photo
//! surface's own tiles, over the core's stacks, is the desktop's test (`app::gpu_tiles_tests`).
//!
//! A test with no adapter prints that it was skipped and asserts nothing: it is not GPU evidence,
//! whatever `cargo test` counts it as.
use super::super::tests::{SIDE, held, identity};
use super::super::{GpuBoundary, GpuRegion, TexelMap};
use super::*;
use luxforge_reference::srgb;

/// This host's default adapter, the one the headless tests draw on, by its backend and name;
/// `None`, having printed that `test` was skipped, without one.
fn host_adapter(test: &str) -> Option<(String, String)> {
    let (_, _, info) = super::super::headless::device(test, wgpu::Limits::default())?;
    Some((format!("{:?}", info.backend), info.name))
}

/// A runner on this host's default adapter, its adapter printed; `None` without one.
fn runner(test: &str) -> Option<TileRunner> {
    let (backend, name) = host_adapter(test)?;
    let runner = TileRunner::open(&backend, &name)
        .unwrap_or_else(|refusal| panic!("{test}: no runner on {backend} {name:?}: {refusal:?}"));
    eprintln!("{test}: the runner's adapter {:?}", runner.adapter());
    Some(runner)
}

/// The identity's plan of the `size` cut of `source` at `origin`, drawn as a tile of its stage
/// is: a region plan of the stage over its own boundary, its texels at the cut's origin.
fn cut_plan(source: &GpuSource, origin: (u32, u32), size: (u32, u32), version: u64) -> GpuPlan {
    let boundary =
        GpuBoundary::derived(source, Derivation::Cut { origin }, size.0, size.1, version)
            .expect("a derived boundary");
    GpuPlan {
        boundary,
        texels: TexelMap {
            origin: [origin.0 as f32, origin.1 as f32],
            step: [1.0, 1.0],
        },
        steps: vec![GpuStep::colour(identity())],
        region: Some(GpuRegion {
            rect: [origin.0, origin.1, origin.0 + size.0, origin.1 + size.1],
            stage: source.stage(),
        }),
    }
}

/// Codes of every channel that vary along both axes, so a texel read from the wrong place is told
/// apart: `width` × `height` pixels, four bytes each.
fn codes(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend([
                ((x * 7 + y * 3) % 256) as u8,
                ((x * 2 + y * 11 + 5) % 256) as u8,
                ((x ^ y) % 256) as u8,
                255,
            ]);
        }
    }
    rgba
}

/// The held texel of a crop window of `width` × `height` that content pixel `(x, y)` shows under
/// EXIF `orientation`.
fn viewed(orientation: u8, (width, height): (u32, u32), x: u32, y: u32) -> (u32, u32) {
    let (w, h) = (width - 1, height - 1);
    match orientation {
        1 => (x, y),
        2 => (w - x, y),
        3 => (w - x, h - y),
        4 => (x, h - y),
        5 => (y, x),
        6 => (y, h - x),
        7 => (w - y, h - x),
        _ => (w - y, x),
    }
}

fn run_codes(
    runner: &mut TileRunner,
    plan: &GpuPlan,
    source: &GpuSource,
    window: [u32; 4],
) -> Vec<u8> {
    match runner.run(plan, source, window, TileEnd::Codes) {
        Ok(TilePixels::Codes(codes)) => codes,
        other => panic!("the codes of {window:?}: {other:?}"),
    }
}

fn run_linear(
    runner: &mut TileRunner,
    plan: &GpuPlan,
    source: &GpuSource,
    window: [u32; 4],
) -> Vec<[f32; 3]> {
    match runner.run(plan, source, window, TileEnd::Linear) {
        Ok(TilePixels::Linear(values)) => values,
        other => panic!("the linear values of {window:?}: {other:?}"),
    }
}

/// The bits of every channel, so a NaN and the sign of a zero count.
fn bits(values: &[[f32; 3]]) -> Vec<[u32; 3]> {
    values.iter().map(|value| value.map(f32::to_bits)).collect()
}

/// A runner holds a window of the source and cuts from it what the whole source cuts, bit for bit,
/// codes and linear values alike: a JPEG's codes over a window away from every edge, and a RAW's
/// planes, viewed from a crop window of them through every orientation. Each pixel is the
/// identity's of its own content pixel: a JPEG code's linear value held as the nearest half float,
/// a RAW's `f32` as it is. The window held is the window's texels alone, kept for the next run of
/// the same window and let go for another.
#[test]
fn a_window_of_the_source_cuts_what_the_whole_source_cuts() {
    let test = "a_window_of_the_source_cuts_what_the_whole_source_cuts";
    let Some(mut runner) = runner(test) else {
        return;
    };
    let (width, height) = (300u32, 200u32);
    let rgba = codes(width, height);
    let source = GpuSource::codes(1, Arc::new(rgba.clone()), width, height).expect("a source");
    let rect = [100u32, 60, 120, 90];
    let origin = (120u32, 70u32);
    let plan = cut_plan(&source, origin, (SIDE, SIDE), 1);
    let whole = [0, 0, width, height];
    let from_whole = run_codes(&mut runner, &plan, &source, whole);
    assert_eq!(
        runner.figures().in_use,
        source.bytes(),
        "the whole source held"
    );
    let from_window = run_codes(&mut runner, &plan, &source, rect);
    assert_eq!(from_window, from_whole, "a JPEG's codes");
    assert_eq!(
        runner.figures().in_use,
        u64::from(rect[2] * rect[3] * 4),
        "the window's texels alone"
    );
    let windowed = source.window(rect).expect("a window");
    assert_eq!(
        run_codes(&mut runner, &plan, &windowed, rect),
        from_whole,
        "a window handed as one"
    );
    let linear_whole = run_linear(&mut runner, &plan, &source, whole);
    let linear_window = run_linear(&mut runner, &plan, &source, rect);
    assert_eq!(bits(&linear_window), bits(&linear_whole), "a JPEG's values");
    for index in 0..(SIDE * SIDE) as usize {
        let (x, y) = (
            origin.0 + index as u32 % SIDE,
            origin.1 + index as u32 / SIDE,
        );
        let at = ((y * width + x) * 4) as usize;
        let values = [0, 1, 2].map(|channel| held(rgba[at + channel]));
        assert_eq!(
            linear_window[index].map(f32::to_bits),
            values.map(f32::to_bits),
            "({x}, {y})"
        );
        let codes = values.map(|value| srgb::code(f64::from(value)));
        assert_eq!(
            from_window[index * 4..index * 4 + 4],
            [codes[0], codes[1], codes[2], 255],
            "({x}, {y})"
        );
    }
    // A RAW's planes: a 300 × 200 crop window of 320 × 210 base planes, viewed through each
    // orientation, a window of the content stage and a cut inside it.
    let base = (320u32, 210u32);
    let crop = [12u32, 5, 300, 200];
    let plane = (base.0 * base.1) as usize;
    let value = |channel: usize, x: u32, y: u32| {
        (channel as f32 + 1.0) * 0.001 * (x as f32) + 0.0007 * (y as f32) - 0.05 * channel as f32
    };
    let mut planes = vec![0f32; 3 * plane];
    for channel in 0..3 {
        for y in 0..base.1 {
            for x in 0..base.0 {
                planes[channel * plane + (y * base.0 + x) as usize] = value(channel, x, y);
            }
        }
    }
    let planes = Arc::new(planes);
    for orientation in 1..=8u8 {
        let version = 10 + u64::from(orientation);
        let source = GpuSource::planes(version, Arc::clone(&planes), base, crop, orientation)
            .expect("a viewed source");
        let stage = source.stage();
        let rect = [stage.0 / 2 - 50, stage.1 / 2 - 40, 100, 80];
        let origin = (rect[0] + 10, rect[1] + 8);
        let plan = cut_plan(&source, origin, (SIDE, SIDE), version);
        let whole = [0, 0, stage.0, stage.1];
        let from_whole = run_linear(&mut runner, &plan, &source, whole);
        let from_window = run_linear(&mut runner, &plan, &source, rect);
        assert_eq!(
            bits(&from_window),
            bits(&from_whole),
            "orientation {orientation}"
        );
        assert_eq!(
            runner.figures().in_use,
            u64::from(rect[2] * rect[3] * 12),
            "orientation {orientation}: the window's planes alone"
        );
        let codes = run_codes(&mut runner, &plan, &source, rect);
        assert_eq!(
            codes,
            run_codes(&mut runner, &plan, &source, whole),
            "orientation {orientation}"
        );
        for index in 0..(SIDE * SIDE) as usize {
            let (x, y) = (
                origin.0 + index as u32 % SIDE,
                origin.1 + index as u32 / SIDE,
            );
            let (hx, hy) = viewed(orientation, (crop[2], crop[3]), x, y);
            let values = [0, 1, 2].map(|channel| value(channel, crop[0] + hx, crop[1] + hy));
            assert_eq!(
                from_window[index].map(f32::to_bits),
                values.map(f32::to_bits),
                "orientation {orientation}: ({x}, {y})"
            );
            let expected = values.map(|value| srgb::code(f64::from(value)));
            assert_eq!(
                codes[index * 4..index * 4 + 3],
                expected,
                "orientation {orientation}: ({x}, {y})"
            );
        }
    }
    eprintln!("{test}: {:?}", runner.figures());
}

/// A run whose charge passes [`GPU_TILE_BUDGET`] is refused with what it asked for, having created
/// and compiled nothing — no window held, no sequence compiled, nothing ever charged — and the
/// charge is the run's every texture and buffer: a whole 8192-pixel JPEG's window of codes, its
/// boundary of half floats, its output's codes and their readback copy, and three buffers of the
/// smallest size. A run within the budget then draws.
#[test]
fn a_tile_runner_refuses_past_its_budget_before_creating_anything() {
    let test = "a_tile_runner_refuses_past_its_budget_before_creating_anything";
    let Some(mut runner) = runner(test) else {
        return;
    };
    let side = runner.device.limits().max_texture_dimension_2d.min(8192);
    // Zeroed and never read: the run is refused before any of it is uploaded.
    let rgba = vec![0u8; side as usize * side as usize * 4];
    let source = GpuSource::codes(1, Arc::new(rgba), side, side).expect("a source");
    let whole = [0, 0, side, side];
    let plan = cut_plan(&source, (0, 0), (side, side), 1);
    let charge = runner
        .charge(&plan, &source, whole, TileEnd::Codes)
        .expect("a charge");
    let pixels = u64::from(side) * u64::from(side);
    assert_eq!(charge, pixels * (4 + 8 + 4 + 4) + 3 * MIN_BUFFER);
    if charge <= GPU_TILE_BUDGET {
        eprintln!("skipped: a device of {side} px textures holds no run past the budget");
        return;
    }
    assert_eq!(
        runner.run(&plan, &source, whole, TileEnd::Codes),
        Err(TileFailure::Budget {
            requested: charge,
            budget: GPU_TILE_BUDGET,
        })
    );
    assert_eq!(
        TileFailure::Budget {
            requested: charge,
            budget: GPU_TILE_BUDGET
        }
        .code(),
        "tiles-budget"
    );
    assert_eq!(
        runner.figures(),
        TileFigures::default(),
        "nothing held, charged or compiled"
    );
    assert!(runner.window.is_none());
    assert_eq!(runner.sequences.len(), 0);
    let small = cut_plan(&source, (0, 0), (SIDE, SIDE), 2);
    let window = [0, 0, SIDE, SIDE];
    let charge = runner
        .charge(&small, &source, window, TileEnd::Codes)
        .expect("a charge");
    let codes = run_codes(&mut runner, &small, &source, window);
    assert_eq!(codes.len(), (SIDE * SIDE * 4) as usize);
    let figures = runner.figures();
    assert_eq!(
        (figures.peak, figures.in_use, figures.compiles, figures.runs),
        (charge, u64::from(SIDE * SIDE * 4), 1, 1)
    );
    runner.release();
    assert_eq!(runner.figures().in_use, 0);
}

/// A device lost while the runner holds a window answers `device-lost` from then on, whether the
/// loss reached the runner's lost callback before the run began or the run itself met it: no run
/// waits for a recovery, and the window it held is let go with the device.
#[test]
fn a_lost_device_answers_device_lost() {
    let test = "a_lost_device_answers_device_lost";
    let lost = TileFailure::Unavailable(TileUnavailable::DeviceLost);
    let rgba = codes(128, 96);
    let source = GpuSource::codes(1, Arc::new(rgba), 128, 96).expect("a source");
    let plan = cut_plan(&source, (16, 8), (SIDE, SIDE), 1);
    let window = [16, 8, SIDE, SIDE];
    // Lost through its callback before the run.
    let Some(mut first) = runner(test) else {
        return;
    };
    run_codes(&mut first, &plan, &source, window);
    first.simulate_device_loss();
    assert!(first.lost(), "the lost callback ran");
    assert_eq!(first.run(&plan, &source, window, TileEnd::Codes), Err(lost));
    assert_eq!(first.figures().in_use, 0, "nothing held of a lost device");
    assert_eq!(
        first.run(&plan, &source, window, TileEnd::Linear),
        Err(lost)
    );
    assert_eq!(lost.code(), "tiles-unavailable");
    // Destroyed with no poll after it, so the run itself meets the loss.
    let Some(mut second) = runner(test) else {
        return;
    };
    run_codes(&mut second, &plan, &source, window);
    second.device.destroy();
    assert_eq!(
        second.run(&plan, &source, window, TileEnd::Codes),
        Err(lost)
    );
    assert!(second.lost(), "the run's wait reached the lost callback");
    assert_eq!(
        second.run(&plan, &source, window, TileEnd::Codes),
        Err(lost)
    );
    assert_eq!(second.figures().in_use, 0);
}

/// An adapter the host does not offer under the backend and name asked for is refused by name —
/// the window's adapter under another backend's name, or another name on its backend — naming
/// what was asked for and what the host offers, and never opened in its place; a launch that
/// refused the GPU stage opens nothing at all; the adapter asked for opens.
#[test]
fn an_adapter_mismatch_is_refused_by_name() {
    let test = "an_adapter_mismatch_is_refused_by_name";
    let Some((backend, name)) = host_adapter(test) else {
        return;
    };
    let offered = adapters::enumerate(adapters::renderer_backends());
    let other = ["Metal", "Vulkan", "Dx12", "Gl"]
        .into_iter()
        .find(|other| adapters::matching(&offered, other, &name).is_none());
    let mut asked = vec![(backend.clone(), "No such adapter".to_owned())];
    asked.extend(other.map(|other| (other.to_owned(), name.clone())));
    for (asked_backend, asked_name) in &asked {
        let refusal = TileRunner::open(asked_backend, asked_name)
            .expect_err("an adapter the host does not offer");
        assert_eq!(refusal.reason, TileUnavailable::AdapterMismatch);
        assert_eq!(refusal.reason.as_str(), "adapter-mismatch");
        for named in [asked_backend, asked_name, &backend, &name] {
            assert!(
                refusal.detail.contains(named.as_str()),
                "{named} in {}",
                refusal.detail
            );
        }
        eprintln!("{test}: {}", refusal.detail);
    }
    let refusal = TileRunner::open_with(true, &backend, &name).expect_err("a refused launch");
    assert_eq!(refusal.reason, TileUnavailable::Refused);
    let runner = TileRunner::open(&backend, &name).expect("the adapter asked for");
    assert_eq!(
        (
            runner.adapter().backend.as_str(),
            runner.adapter().name.as_str()
        ),
        (backend.as_str(), name.as_str())
    );
    eprintln!("{test}: the runner's adapter {:?}", runner.adapter());
}

/// What the runner cannot draw is refused before anything is created: a boundary of texels rather
/// than a cut, a cut of another source, a cut that leaves the window it is asked to hold, a window
/// that leaves the source, and clipping marks, whose colours are not codes; and a resident source
/// whose pixels were let go is `source-missing` until a window of it is held.
#[test]
fn only_a_cut_of_its_source_inside_its_window_runs() {
    let test = "only_a_cut_of_its_source_inside_its_window_runs";
    let Some(mut runner) = runner(test) else {
        return;
    };
    let failed = Err(TileFailure::Plan(GpuFallback::PipelineFailed));
    let rgba = Arc::new(codes(128, 96));
    let source = GpuSource::codes(1, Arc::clone(&rgba), 128, 96).expect("a source");
    let plan = cut_plan(&source, (16, 8), (SIDE, SIDE), 1);
    let window = [16, 8, SIDE, SIDE];
    let texels = GpuPlan {
        boundary: GpuBoundary::from_linear(
            super::super::BoundaryFormat::Half,
            SIDE,
            SIDE,
            1,
            std::iter::repeat_n([0.5; 4], (SIDE * SIDE) as usize),
        )
        .expect("a boundary of texels"),
        ..plan.clone()
    };
    let another = GpuSource::codes(2, Arc::clone(&rgba), 128, 96).expect("another source");
    let marked = GpuPlan {
        steps: vec![
            GpuStep::colour(identity()),
            GpuStep::Clipping(super::super::ClipMarks {
                shadows: true,
                highlights: true,
                shadow_below: 0.0,
                highlight_from: 1.0,
                palette: [[0, 0, 255, 255], [255, 0, 0, 255], [255, 0, 255, 255]],
            }),
        ],
        ..plan.clone()
    };
    for (what, plan, source, window) in [
        ("texels", &texels, &source, window),
        ("another source", &plan, &another, window),
        ("a cut past the window", &plan, &source, [20, 8, SIDE, SIDE]),
        (
            "a window past the source",
            &plan,
            &source,
            [16, 8, 120, SIDE],
        ),
        ("clipping marks", &marked, &source, window),
    ] {
        assert_eq!(
            runner.run(plan, source, window, TileEnd::Codes),
            failed,
            "{what}"
        );
    }
    assert_eq!(
        runner.figures(),
        TileFigures::default(),
        "nothing held or compiled"
    );
    let resident = source.resident();
    assert_eq!(
        runner.run(&plan, &resident, window, TileEnd::Codes),
        Err(TileFailure::Plan(GpuFallback::SourceMissing))
    );
    let codes = run_codes(&mut runner, &plan, &source, window);
    assert_eq!(
        run_codes(&mut runner, &plan, &resident, window),
        codes,
        "the window held serves its resident source"
    );
}

/// The compile cache keeps at most its bound, the least recently found or kept evicted first.
#[test]
fn the_compile_cache_keeps_the_most_recently_run_sequences_within_its_bound() {
    let mut cache = Lru::new(TILE_PIPELINE_CACHE);
    for key in 0..TILE_PIPELINE_CACHE {
        cache.insert(key, key * 10);
    }
    assert_eq!(cache.find(|key| *key == 0), Some(&0), "found, so used now");
    cache.insert(TILE_PIPELINE_CACHE, 0);
    assert_eq!(cache.len(), TILE_PIPELINE_CACHE);
    assert!(cache.find(|key| *key == 0).is_some(), "used since");
    assert!(
        cache.find(|key| *key == 1).is_none(),
        "the least recently used"
    );
    assert!(cache.find(|key| *key == TILE_PIPELINE_CACHE).is_some());
}

/// A runner moves to the thread that owns it.
#[test]
fn a_tile_runner_moves_to_its_thread() {
    fn sent<T: Send>() {}
    sent::<TileRunner>();
}
