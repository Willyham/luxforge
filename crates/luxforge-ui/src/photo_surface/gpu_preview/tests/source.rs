//! The prepared source held on the GPU and the boundaries derived from it
//! (`docs/design/gpu-preview.md`, "The GPU source"), on a headless device the test creates, read
//! back through the photograph's real draw: the upload spread over frames, charged and retired;
//! a cut and a reduction across the seams of a source held in tiles; and the fallbacks a derived
//! boundary names until its source is held.
use super::*;

/// Codes of every channel that vary along both axes, so a texel read from the wrong place, tile or
/// orientation is told apart: `width` × `height` pixels, four bytes each.
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

/// The codes a texel of value `linear`, held as the nearest half float, is encoded to.
fn encoded(linear: f64) -> u8 {
    srgb::code(f64::from(half::f16::from_f32(linear as f32).to_f32()))
}

/// The identity plan over `boundary`, a whole frame.
fn over(boundary: &GpuBoundary) -> GpuPlan {
    plan(boundary, vec![identity()])
}

/// Surface [`ID`]'s photograph with `plan`, handing `source` to the pipeline.
fn handing(plan: Option<GpuPlan>, source: Option<&GpuSource>) -> PhotoPrimitive {
    PhotoPrimitive {
        source: source.cloned(),
        ..primitive(ID, plan)
    }
}

/// A source past a frame's upload arrives over several frames, a frame's rows at a time, charged in
/// full before any is written; a boundary derived from it names the upload until its last rows are
/// written, then draws the source's codes through the identity, exactly; a boundary of a source the
/// pipeline does not hold names that; and a source no surface hands retires at the end of the
/// frame, its charge with it.
#[test]
fn a_source_upload_is_spread_charged_and_retired() {
    let test = "a_source_upload_is_spread_charged_and_retired";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (width, height) = (256u32, 256u32);
    let rgba = codes(width, height);
    let source =
        GpuSource::codes(7, Arc::new(rgba.clone()), width, height).expect("a whole source");
    assert_eq!(source.bytes(), u64::from(width * height * 4));
    // Sixty-four rows a frame: the source arrives over four frames.
    let row = u64::from(width) * 4;
    pipeline.set_upload_per_frame(64 * row);
    let cut = |version: u64, source: &GpuSource| {
        GpuBoundary::derived(
            source,
            Derivation::Cut { origin: (96, 40) },
            SIDE,
            SIDE,
            version,
        )
        .expect("a derived boundary")
    };
    let boundary = cut(3, &source);
    let expected: Vec<[u8; 3]> = (0..SIDE * SIDE)
        .map(|index| {
            let (x, y) = (96 + index % SIDE, 40 + index / SIDE);
            let at = ((y * width + x) * 4) as usize;
            [0, 1, 2].map(|channel| encoded(srgb::decode(rgba[at + channel])))
        })
        .collect();
    let staged = |pipeline: &PhotoPipeline| diagnostics(pipeline, ID).gpu_preview_staged_bytes;
    let before = staged(&pipeline);
    for rows in [64u64, 128, 192] {
        let drawn = paint(
            &device,
            &queue,
            &mut pipeline,
            &handing(Some(over(&boundary)), Some(&source)),
        );
        pipeline.trim();
        assert_cpu_frame(&drawn);
        let seen = diagnostics(&pipeline, ID);
        assert_eq!(
            seen.gpu_source,
            Some(SourceFigures {
                version: 7,
                bytes: source.bytes(),
                uploaded: rows * row,
                ready: false,
            }),
            "after {rows} rows"
        );
        assert_eq!(
            seen.gpu_fallback,
            Some(GpuFallback::SourceUploading {
                uploaded: rows * row,
                bytes: source.bytes(),
            })
        );
        assert!(
            seen.gpu_preview_in_use_bytes >= source.bytes(),
            "charged in full from its first frame"
        );
    }
    let drawn = paint(
        &device,
        &queue,
        &mut pipeline,
        &handing(Some(over(&boundary)), Some(&source)),
    );
    pipeline.trim();
    assert_codes(&drawn, &expected);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.gpu_fallback, None);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert!(seen.gpu_source.is_some_and(|held| held.ready));
    assert_eq!(staged(&pipeline) - before, source.bytes(), "every row once");
    assert_eq!(seen.gpu_source_derived, 1);
    // Let go once held, the source is still drawn from: the pipeline holds it.
    let resident = source.resident();
    let moved = cut(4, &resident);
    assert!(
        !paint(
            &device,
            &queue,
            &mut pipeline,
            &handing(Some(over(&moved)), Some(&resident))
        )
        .is_empty()
    );
    pipeline.trim();
    assert_eq!(diagnostics(&pipeline, ID).gpu_fallback, None);
    assert_eq!(diagnostics(&pipeline, ID).gpu_source_derived, 2);
    // A boundary of a source the pipeline does not hold.
    let other = GpuSource::codes(8, Arc::new(rgba.clone()), width, height).expect("a source");
    assert_cpu_frame(&paint(
        &device,
        &queue,
        &mut pipeline,
        &handing(Some(over(&cut(5, &other))), Some(&resident)),
    ));
    pipeline.trim();
    assert_eq!(
        diagnostics(&pipeline, ID).gpu_fallback,
        Some(GpuFallback::SourceMissing)
    );
    // No surface hands it: it retires at the frame's end, its charge with it.
    assert_cpu_frame(&paint(&device, &queue, &mut pipeline, &handing(None, None)));
    pipeline.trim();
    assert_eq!(diagnostics(&pipeline, ID).gpu_source, None);
    settle(&pipeline);
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_in_use_bytes, 0);
}

/// The texel a view of `held` texels under EXIF `orientation` shows at content pixel `(x, y)`, as
/// the core's view maps it: the reference the GPU's map is held to.
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

/// A source wider and taller than the device's largest texture is held in tiles that meet with no
/// seam: a JPEG's cut straddling both seams draws each texel's own code, and a RAW's planes, viewed
/// through every orientation from a crop window of them, draw each content pixel's own value, on a
/// device whose largest texture is 160 px, so a 300 × 200 window spans two tiles each way.
#[test]
fn a_source_past_the_texture_limit_spans_tiles_without_a_seam() {
    let test = "a_source_past_the_texture_limit_spans_tiles_without_a_seam";
    let limits = wgpu::Limits {
        max_texture_dimension_2d: 160,
        ..wgpu::Limits::default()
    };
    let Some((device, queue)) = headless_with(test, limits) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (width, height) = (300u32, 200u32);
    let rgba = codes(width, height);
    let source = GpuSource::codes(1, Arc::new(rgba.clone()), width, height).expect("a source");
    // Both seams, at 160, fall inside the cut.
    let origin = (130, 130);
    let boundary = GpuBoundary::derived(&source, Derivation::Cut { origin }, SIDE, SIDE, 1)
        .expect("a derived boundary");
    let primitive = handing(Some(over(&boundary)), Some(&source));
    // The source arrives in its first frame at the recorded bound.
    let drawn = paint(&device, &queue, &mut pipeline, &primitive);
    pipeline.trim();
    let expected: Vec<[u8; 3]> = (0..SIDE * SIDE)
        .map(|index| {
            let (x, y) = (origin.0 + index % SIDE, origin.1 + index / SIDE);
            let at = ((y * width + x) * 4) as usize;
            [0, 1, 2].map(|channel| encoded(srgb::decode(rgba[at + channel])))
        })
        .collect();
    assert_codes(&drawn, &expected);
    // A RAW's planes: a 300 × 200 crop window of 320 × 210 base planes, viewed through each
    // orientation, its cut straddling the seams of the window as the planes lie.
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
        // A cut across the middle of the content stage, which reaches both seams of the window
        // whatever the orientation.
        let origin = ((stage.0 - SIDE) / 2, (stage.1 - SIDE) / 2);
        // Other texels, so another boundary version, as a caller names it.
        let boundary =
            GpuBoundary::derived(&source, Derivation::Cut { origin }, SIDE, SIDE, version)
                .expect("a derived boundary");
        let drawn = paint(
            &device,
            &queue,
            &mut pipeline,
            &handing(Some(over(&boundary)), Some(&source)),
        );
        pipeline.trim();
        let expected: Vec<[u8; 3]> = (0..SIDE * SIDE)
            .map(|index| {
                let (x, y) = (origin.0 + index % SIDE, origin.1 + index / SIDE);
                let (hx, hy) = viewed(orientation, (crop[2], crop[3]), x, y);
                [0, 1, 2].map(|channel| {
                    srgb::code(f64::from(value(channel, crop[0] + hx, crop[1] + hy)))
                })
            })
            .collect();
        assert_codes(&drawn, &expected);
    }
    settle(&pipeline);
}

/// A reduction across the seams of a source held in tiles is the area average of its codes, with
/// the coverage the CPU's proxy build weighs them by, quantized and decoded again as the CPU's
/// proxy is: within a code of an `f64` reference average everywhere, on a device whose largest
/// texture is 160 px.
#[test]
fn a_reduction_across_tiles_is_the_area_average_within_a_code() {
    let test = "a_reduction_across_tiles_is_the_area_average_within_a_code";
    let limits = wgpu::Limits {
        max_texture_dimension_2d: 160,
        ..wgpu::Limits::default()
    };
    let Some((device, queue)) = headless_with(test, limits) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (width, height) = (300u32, 200u32);
    let rgba = codes(width, height);
    let source = GpuSource::codes(1, Arc::new(rgba.clone()), width, height).expect("a source");
    // The proxy stage 128 × 85, the window its 64 × 64 boundary holds at (40, 12).
    let (output, origin) = ((128u32, 85u32), (40u32, 12u32));
    let coverage = |source: u32, output: u32| {
        let ratio = f64::from(source) / f64::from(output);
        let (mut first, mut offsets, mut weights) = (Vec::new(), vec![0u32], Vec::new());
        let mut exact = Vec::new();
        for index in 0..output {
            let (start, end) = (f64::from(index) * ratio, f64::from(index + 1) * ratio);
            let begin = (start.floor() as u32).min(source - 1);
            let last = (end.ceil() as u32).clamp(begin + 1, source);
            first.push(begin);
            let mut spans = Vec::new();
            for sample in begin..last {
                let low = start.max(f64::from(sample));
                let high = end.min(f64::from(sample + 1));
                let weight = (high - low).max(0.0) / ratio;
                weights.push(weight as f32);
                spans.push((sample, weight));
            }
            offsets.push(weights.len() as u32);
            exact.push(spans);
        }
        (
            AxisCoverage {
                first,
                offsets,
                weights,
            },
            exact,
        )
    };
    let (across, across_exact) = coverage(width, output.0);
    let (down, down_exact) = coverage(height, output.1);
    let reduction = Reduction {
        origin,
        across,
        down,
    };
    let boundary = GpuBoundary::derived(
        &source,
        Derivation::Reduce(Arc::new(reduction)),
        SIDE,
        SIDE,
        1,
    )
    .expect("a derived boundary");
    let drawn = paint(
        &device,
        &queue,
        &mut pipeline,
        &handing(Some(over(&boundary)), Some(&source)),
    );
    pipeline.trim();
    let mut exact = 0;
    for index in 0..(SIDE * SIDE) as usize {
        let (x, y) = (
            origin.0 as usize + index % SIDE as usize,
            origin.1 as usize + index / SIDE as usize,
        );
        // The CPU proxy's code: the area average of the decoded codes, quantized.
        let reference: [u8; 3] = [0, 1, 2].map(|channel| {
            let mut sum = 0.0;
            for (sy, wy) in &down_exact[y] {
                for (sx, wx) in &across_exact[x] {
                    let at = ((sy * width + sx) * 4) as usize + channel;
                    sum += wx * wy * srgb::decode(rgba[at]);
                }
            }
            srgb::code(sum)
        });
        // The boundary holds the proxy code's linear value as a half, which the identity encodes.
        let held = reference.map(|code| encoded(srgb::decode(code)));
        let pixel = &drawn[index * 4..index * 4 + 4];
        let gpu = [pixel[2], pixel[1], pixel[0]];
        for channel in 0..3 {
            assert!(
                gpu[channel].abs_diff(held[channel]) <= 1,
                "texel {index}, channel {channel}: {} against {}",
                gpu[channel],
                held[channel]
            );
        }
        exact += usize::from(gpu == held);
    }
    eprintln!(
        "{test}: {exact} of {} texels equal to the f64 reference's codes",
        SIDE * SIDE
    );
    settle(&pipeline);
}

/// A boundary derived from the source draws from the slot that holds it, a tick changing only its
/// words and deriving nothing again, and a plan of another output or tail over it refits the slot
/// around the boundary it holds; a sequence still compiling leaves the slot, and the boundary it
/// holds, as they were. A slot let go — by a frame with no plan — derives the boundary again from
/// the source the pipeline still holds, and draws it once more: nothing is asked of the caller.
#[test]
fn a_derived_boundary_draws_from_the_slot_that_holds_it() {
    let test = "a_derived_boundary_draws_from_the_slot_that_holds_it";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let rgba = codes(SIDE, SIDE);
    let source = GpuSource::codes(5, Arc::new(rgba.clone()), SIDE, SIDE).expect("a whole source");
    let boundary = GpuBoundary::derived(&source, Derivation::Cut { origin: (0, 0) }, SIDE, SIDE, 3)
        .expect("a derived boundary");
    let expected: Vec<[u8; 3]> = rgba
        .chunks_exact(4)
        .map(|pixel| [0, 1, 2].map(|channel| encoded(srgb::decode(pixel[channel]))))
        .collect();
    let with = |plan: GpuPlan| handing(Some(plan), Some(&source));
    let drawn = paint(&device, &queue, &mut pipeline, &with(over(&boundary)));
    assert_codes(&drawn, &expected);
    let seen = diagnostics(&pipeline, ID);
    let (derived, slot_bytes) = (seen.gpu_source_derived, seen.gpu_preview_in_use_bytes);
    assert_eq!(derived, 1);
    // The next tick over it is drawn from the slot.
    let drawn = paint(&device, &queue, &mut pipeline, &with(over(&boundary)));
    assert_codes(&drawn, &expected);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert_eq!(seen.gpu_ready_boundary, Some(3));
    assert_eq!(seen.gpu_source_derived, derived, "derived once");
    // A sequence the frame finds still compiling keeps the slot and the boundary in it.
    let scaled = plan(&boundary, vec![scale(0.5)]);
    paint_prepared(&device, &queue, &mut pipeline, &with(scaled.clone()));
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.gpu_fallback, Some(GpuFallback::Compiling));
    assert_eq!(
        seen.gpu_preview_in_use_bytes, slot_bytes,
        "the slot is kept"
    );
    pipeline.compile_now(&device, &scaled);
    paint(&device, &queue, &mut pipeline, &with(scaled));
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(
        seen.drawn_path,
        Some(DrawingPath::Gpu),
        "drawn from the slot"
    );
    assert_eq!(seen.gpu_fallback, None);
    // A plan of another shape over the same boundary — here the identity tail a colour step after
    // a stack's last spatial one brings — refits the slot and keeps the boundary it holds.
    let mut tailed = over(&boundary);
    tailed.steps.push(GpuStep::Geometry(GpuTail::affine(
        (SIDE, SIDE),
        [0, 0, SIDE, SIDE],
        false,
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
    )));
    tailed.steps.push(GpuStep::colour(identity()));
    pipeline.compile_now(&device, &tailed);
    let drawn = paint(&device, &queue, &mut pipeline, &with(tailed));
    assert_codes(&drawn, &expected);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu), "the kept boundary");
    assert_eq!(seen.gpu_fallback, None);
    assert_eq!(seen.gpu_ready_boundary, Some(3));
    // A frame with no plan lets the slot go, and the source stays while it is handed: the boundary
    // is derived from it again.
    assert_cpu_frame(&paint(
        &device,
        &queue,
        &mut pipeline,
        &handing(None, Some(&source)),
    ));
    let drawn = paint(&device, &queue, &mut pipeline, &with(over(&boundary)));
    assert_codes(&drawn, &expected);
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.gpu_fallback, None);
    assert!(seen.gpu_source_derived > derived, "derived again");
    settle(&pipeline);
}
