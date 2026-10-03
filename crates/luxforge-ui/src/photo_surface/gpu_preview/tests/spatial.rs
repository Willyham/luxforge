//! The spatial step's own tests: the spatial convention checked on a program alone, the modules the
//! surface assembles for its passes and frame, and, on a headless device, passes that fill planes
//! before the frame's pass reads them, charged to the budget and released with the slot.
use super::super::spatial::{Slots, fragment_declarations, pass_module};
use super::*;

/// A hand-supplied spatial program under the convention: a pass that copies its unit's input into
/// a plane, a horizontal box mean of a plane of radius word 0, a workgroup pass that sums its lanes
/// through the shared scratch, and an apply that shows the mean and the lanes' sum.
const PROGRAM: &str = "\
fn lf_test_copy(at: vec2<i32>, words: u32, block: u32) {
    lf_store(at, vec4<f32>(lf_source(at), 1.0));
}

fn lf_test_mean_x(at: vec2<i32>, words: u32, block: u32) {
    let r = i32(lf_word(words));
    var sum = vec4<f32>(0.0);
    for (var dx = -r; dx <= r; dx++) {
        sum += lf_plane(0u, at + vec2<i32>(dx, 0));
    }
    lf_store(at, sum / f32(2 * r + 1));
}

fn lf_test_lanes(at: vec2<i32>, words: u32, block: u32) {
    let lane = u32(at.x);
    lf_shared[lane] = f32(lane);
    workgroupBarrier();
    if lane == 0u {
        var total = 0.0;
        for (var other = 0u; other < 256u; other++) {
            total += lf_shared[other];
        }
        lf_store(vec2<i32>(0), vec4<f32>(total / 65280.0));
    }
}

fn lf_test_show(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    let mean = lf_plane(planes, at);
    let half = lf_plane(planes + 1u, vec2<i32>(0)).x;
    return vec3<f32>(mean.x, mean.y, half * lf_f32(words));
}
";

/// The radius of the test mean.
const RADIUS: u32 = 3;
/// The apply's word: what the lanes' half is scaled by.
const SCALE: f32 = 0.5;

fn test_spatial() -> GpuSpatial {
    GpuSpatial {
        program: GpuProgram {
            words: vec![RADIUS, SCALE.to_bits()],
            ..GpuProgram::new("lf_test", PROGRAM)
        },
        planes: vec![
            GpuPlane {
                format: PlaneFormat::Colour,
                size: PlaneSize::Reduced(1),
            },
            GpuPlane {
                format: PlaneFormat::Quad,
                size: PlaneSize::Reduced(1),
            },
            GpuPlane {
                format: PlaneFormat::Quad,
                size: PlaneSize::Fixed {
                    width: 1,
                    height: 1,
                },
            },
        ],
        passes: vec![
            GpuPass {
                kernel: Cow::Borrowed("lf_test_copy"),
                inputs: vec![],
                output: 0,
                words: 0,
                source: 0,
                shape: PassShape::Texels { span: [1, 1] },
                unit: 0,
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_mean_x"),
                inputs: vec![0],
                output: 1,
                words: 0,
                source: 0,
                shape: PassShape::Texels { span: [1, 1] },
                unit: 0,
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_lanes"),
                inputs: vec![],
                output: 2,
                words: 0,
                source: 0,
                shape: PassShape::Workgroup,
                unit: 0,
            },
        ],
        applies: vec![GpuApply {
            function: Cow::Borrowed("lf_test_show"),
            planes: vec![1, 2],
            words: 1,
        }],
        clamps: true,
        mask: None,
        halos: Vec::new(),
    }
}

/// A colour step that halves, then the test's spatial step.
fn spatial_plan(boundary: &GpuBoundary) -> GpuPlan {
    GpuPlan {
        boundary: boundary.clone(),
        texels: TexelMap::IDENTITY,
        steps: vec![
            GpuStep::colour(scale(0.5)),
            GpuStep::Spatial(Box::new(test_spatial())),
        ],
        region: None,
    }
}

// ---- Without a device -------------------------------------------------------------------------

/// A spatial step's program is checked alone, under the stub declarations, with every kernel and
/// apply its passes name at its own signature and every index in range.
#[test]
fn a_spatial_step_is_checked_against_the_spatial_convention() {
    validate_step(&GpuStep::Spatial(Box::new(test_spatial()))).expect("the test step");
    let refused = |what: &str, change: &dyn Fn(&mut GpuSpatial), expected: &str| {
        let mut spatial = test_spatial();
        change(&mut spatial);
        let error = validate_step(&GpuStep::Spatial(Box::new(spatial))).unwrap_err();
        assert!(error.contains(expected), "{what}: {error}");
    };
    refused(
        "a kernel it does not declare",
        &|spatial| spatial.passes[0].kernel = Cow::Borrowed("lf_test_absent"),
        "declares no function",
    );
    refused(
        "a kernel not named after the program",
        &|spatial| spatial.passes[0].kernel = Cow::Borrowed("other_copy"),
        "does not start with its program's name",
    );
    refused(
        "an apply's signature for a kernel",
        &|spatial| spatial.passes[0].kernel = Cow::Borrowed("lf_test_show"),
        "must be fn lf_test_show(at: vec2<i32>",
    );
    refused(
        "a kernel's signature for an apply",
        &|spatial| spatial.applies[0].function = Cow::Borrowed("lf_test_copy"),
        "must be fn lf_test_copy(rgb: vec3<f32>",
    );
    refused(
        "a pass reading its own output",
        &|spatial| spatial.passes[1].inputs = vec![1],
        "its own output",
    );
    refused(
        "a pass reading five planes",
        &|spatial| spatial.passes[1].inputs = vec![0, 0, 0, 0, 0],
        "more than 4 planes",
    );
    refused(
        "a plane the step does not declare",
        &|spatial| spatial.applies[0].planes = vec![7],
        "does not hold",
    );
    refused(
        "an apply the step does not hold",
        &|spatial| spatial.passes[0].source = 2,
        "does not hold",
    );
    refused(
        "an empty span",
        &|spatial| spatial.passes[0].shape = PassShape::Texels { span: [0, 1] },
        "empty span",
    );
    refused(
        "a global of its own",
        &|spatial| {
            spatial.program.source =
                Cow::Owned(format!("var<private> lf_test_state: f32;\n{PROGRAM}"))
        },
        "global variable",
    );
    refused(
        "a helper not named after the program",
        &|spatial| spatial.program.source = Cow::Owned(format!("fn helper() {{}}\n{PROGRAM}")),
        "does not start with its entry's name",
    );
    refused(
        "a name a surface name starts with",
        &|spatial| spatial.program.entry = Cow::Borrowed("lf_pl"),
        "surface's own",
    );
}

/// The frame's module binds every apply's planes from slot 0 and declares stubs for what only a
/// pass uses; each pass's module binds its inputs, then the planes of the applies its source runs,
/// then its output, and runs the colour steps before the spatial one in its source. Every module
/// validates with the naga wgpu uses.
#[test]
fn the_frame_and_every_pass_assemble_into_modules_that_validate() {
    let boundary = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        1,
        1,
        1,
        [[0.0; 4]],
    )
    .unwrap();
    let plan = spatial_plan(&boundary);
    let frame = assemble(&plan.steps).expect("the frame's module");
    validate(&frame).expect("the frame's module validates");
    assert!(frame.contains("@group(1) @binding(1) var lf_plane_1"));
    assert!(frame.contains("rgb = lf_test_show(rgb, vec2<i32>(texel)"));
    let (_, slots) = fragment_declarations(&plan.steps);
    assert_eq!(slots.planes(), &[(1, 1), (1, 2)]);
    for pass in 0..3 {
        let GpuStep::Spatial(spatial) = &plan.steps[1] else {
            unreachable!()
        };
        let (module, slots): (String, Slots) =
            pass_module(&plan.steps, 1, &spatial.passes[pass]).expect("a pass's module");
        validate(&module).unwrap_or_else(|error| panic!("pass {pass}: {error}"));
        assert!(
            module.contains("rgb = scale(rgb"),
            "pass {pass} runs the colour step"
        );
        assert!(
            module.contains("rgb = clamp(rgb"),
            "pass {pass} clamps its input"
        );
        assert_eq!(slots.len(), spatial.passes[pass].inputs.len());
    }
}

/// A reduced plane covers the stage's blocks the boundary reaches, anchored at the stage origin.
#[test]
fn a_reduced_plane_holds_the_stage_blocks_its_boundary_reaches() {
    let plane = |s| GpuPlane {
        format: PlaneFormat::Scalar,
        size: PlaneSize::Reduced(s),
    };
    assert_eq!(plane(1).extent((0, 0), (37, 21)), (37, 21));
    assert_eq!(plane(4).extent((0, 0), (37, 21)), (10, 6));
    // A window from stage pixel (5, 3) holds blocks 1..=10 across and 0..=5 down.
    assert_eq!(plane(4).extent((5, 3), (37, 21)), (10, 6));
    assert_eq!(plane(4).extent((2, 0), (39, 21)), (11, 6));
    assert_eq!(plane(16).extent((0, 0), (37, 21)), (3, 2));
    let fixed = GpuPlane {
        format: PlaneFormat::Quad,
        size: PlaneSize::Fixed {
            width: 1,
            height: 1,
        },
    };
    assert_eq!(
        (
            fixed.extent((9, 9), (500, 500)),
            fixed.bytes((9, 9), (500, 500))
        ),
        ((1, 1), 16)
    );
    assert_eq!(plane(1).bytes((0, 0), (10, 10)), 400);
}

// ---- On a headless device ---------------------------------------------------------------------

/// The test plan's frame by the independent reference: each channel halved and clamped, the
/// horizontal mean over seven texels of the red and green, edge-clamped, and the lanes' half
/// scaled into the blue, then clamped and encoded.
fn expected_codes(values: &[[f32; 3]]) -> Vec<[u8; 3]> {
    expected_codes_with(values, 0.5, RADIUS, SCALE)
}

/// The codes the test plan draws with its colour step's `factor`, the mean's `radius` and the
/// apply's `scale`.
fn expected_codes_with(values: &[[f32; 3]], factor: f32, radius: u32, scale: f32) -> Vec<[u8; 3]> {
    let side = SIDE as i64;
    let input = |x: i64, y: i64, channel: usize| {
        let x = x.clamp(0, side - 1);
        let value = values[(y * side + x) as usize][channel];
        // The pass held the copy as a half float.
        let halved = f64::from((value * factor).clamp(0.0, 1.0));
        f64::from(half::f16::from_f64(halved).to_f32())
    };
    (0..side * side)
        .map(|index| {
            let (x, y) = (index % side, index / side);
            let mean = |channel| {
                (-(radius as i64)..=radius as i64)
                    .map(|dx| input(x + dx, y, channel))
                    .sum::<f64>()
                    / f64::from(2 * radius + 1)
            };
            [
                srgb::code(mean(0).clamp(0.0, 1.0)),
                srgb::code(mean(1).clamp(0.0, 1.0)),
                srgb::code(0.5 * f64::from(scale)),
            ]
        })
        .collect()
}

/// The boundary's values the frame was built from, as halves widened.
fn boundary_values() -> (GpuBoundary, Vec<[f32; 3]>) {
    let mut values = Vec::with_capacity((SIDE * SIDE) as usize);
    for index in 0..SIDE * SIDE {
        let (x, y) = (index % SIDE, index / SIDE);
        values.push(if y == SIDE - 1 {
            // Past white and below black, which the step's clamp holds in range.
            [3.0, -1.0, 0.5]
        } else {
            let code = |a: u32, b: u32| ((x * a + y * 3 + b) % 256) as u8;
            [held(code(5, 0)), held(code(11, 7)), held(code(3, 1))]
        });
    }
    let boundary = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        SIDE,
        SIDE,
        1,
        values.iter().map(|[r, g, b]| [*r, *g, *b, 1.0]),
    )
    .unwrap();
    let values = values
        .iter()
        .map(|rgb| rgb.map(|value| half::f16::from_f32(value).to_f32()))
        .collect();
    (boundary, values)
}

/// Every pass runs, in order, before the frame's pass, over the colour steps before the spatial one
/// and its clamp; the workgroup pass's lanes share their scratch; the frame's apply reads what the
/// passes wrote.
#[test]
fn a_spatial_step_runs_its_passes_before_the_frame_that_applies_them() {
    let test = "a_spatial_step_runs_its_passes_before_the_frame_that_applies_them";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, values) = boundary_values();
    let drawn = paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(spatial_plan(&boundary))),
    );
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(
        (seen.drawn_path, seen.gpu_fallback),
        (Some(DrawingPath::Gpu), None)
    );
    let expected = expected_codes(&values);
    let mut largest = 0;
    for (index, (pixel, [r, g, b])) in drawn.chunks_exact(4).zip(&expected).enumerate() {
        for (drawn, wanted) in [(pixel[2], *r), (pixel[1], *g), (pixel[0], *b)] {
            let difference = drawn.abs_diff(wanted);
            assert!(
                difference <= 1,
                "texel ({}, {}): {drawn} against {wanted}",
                index as u32 % SIDE,
                index as u32 / SIDE
            );
            largest = largest.max(difference);
        }
    }
    eprintln!("{test}: {} texels within {largest} code", expected.len());
}

/// The planes are charged with the slot and released with it; a plan whose planes would pass the
/// budget takes the CPU path and names it.
#[test]
fn spatial_planes_are_charged_released_and_refused_past_the_budget() {
    let test = "spatial_planes_are_charged_released_and_refused_past_the_budget";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, _) = boundary_values();
    let plan = spatial_plan(&boundary);
    // The three planes, and each of the three passes' 256-byte slice of the parameters.
    let planes = 64 * 64 * 8 + 64 * 64 * 16 + 16 + 3 * 256;
    // The colour step before the spatial one is the chain's first link: its intermediate, the
    // boundary's size at its eight bytes a texel, and its words and blocks buffers.
    let link = 64 * 64 * 8 + 2 * 1024;
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(plan.clone())),
    );
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert_eq!(seen.gpu_preview_in_use_bytes, SLOT_BYTES + planes + link);
    // The figure a qualification report states is the slot's own.
    assert_eq!(
        slot_charge(&device, &plan),
        Ok(seen.gpu_preview_in_use_bytes)
    );
    // The plan stops: the slot and its planes retire, and the figure returns to zero.
    paint(&device, &queue, &mut pipeline, &primitive(ID, None));
    settle(&pipeline);
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_in_use_bytes, 0);
    // A budget that holds the slot but not its planes.
    pipeline.set_gpu_budget(SLOT_BYTES + planes - 1);
    assert_cpu_frame(&paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(plan)),
    ));
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Cpu));
    assert!(
        matches!(seen.gpu_fallback, Some(GpuFallback::BudgetExceeded { requested, .. })
            if requested == planes),
        "{:?}",
        seen.gpu_fallback
    );
    settle(&pipeline);
    assert_eq!(diagnostics(&pipeline, ID).gpu_preview_in_use_bytes, 0);
}

/// A plan whose planes the budget holds only once the ones they replace have gone, drawn straight
/// after the smaller plan: the planes it replaces stay charged until their retirement ends, when
/// the GPU is done with them, so the larger plan's first frame is the CPU's, naming the budget,
/// and nothing of it is created. Once the retirement ends, the next frame holds the larger planes
/// and every frame after it draws them on the GPU, the slot charged the same: one frame falls
/// back, and nothing is allocated again or released after it. The in-use figure never passes the
/// budget. A drag at 100% whose slot replaces a Fit slot, or another region's, meets this as its
/// boundary arrives.
#[test]
fn a_larger_plan_waits_for_the_planes_it_replaces_then_holds_its_own() {
    let test = "a_larger_plan_waits_for_the_planes_it_replaces_then_holds_its_own";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, _) = boundary_values();
    let single = spatial_plan(&boundary);
    // The same chain, its spatial step's copy held in a plane twice as wide: the last link's planes
    // are replaced, and the colour link before it is kept.
    let mut wider = test_spatial();
    wider.planes[0].format = PlaneFormat::Quad;
    let larger = GpuPlan {
        steps: vec![
            GpuStep::colour(scale(0.5)),
            GpuStep::Spatial(Box::new(wider)),
        ],
        ..single.clone()
    };
    let planes = |plan: &GpuPlan| {
        super::super::spatial::PlanesKey::of(&plan.steps, (SIDE, SIDE), (0, 0))
            .expect("planes")
            .bytes()
    };
    let (small, large) = (planes(&single), planes(&larger));
    // The colour step before the spatial one is the chain's first link: its intermediate, the
    // boundary's size at its eight bytes a texel, and its words and blocks buffers.
    let link = 64 * 64 * 8 + 2 * 1024;
    // The slot with either plan's planes, never both.
    let budget = SLOT_BYTES + link + large;
    assert!(SLOT_BYTES + link + small + large > budget);
    pipeline.set_gpu_budget(budget);
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(single.clone())),
    );
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert_eq!(seen.gpu_preview_in_use_bytes, SLOT_BYTES + link + small);
    // Straight after it: the smaller planes still retire.
    let first = paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(larger.clone())),
    );
    let seen = diagnostics(&pipeline, ID);
    match seen.drawn_path {
        Some(DrawingPath::Cpu) => {
            assert_cpu_frame(&first);
            assert!(
                matches!(seen.gpu_fallback, Some(GpuFallback::BudgetExceeded { requested, .. })
                    if requested == large),
                "{:?}",
                seen.gpu_fallback
            );
            eprintln!("{test}: the first frame is the CPU's, naming the budget");
        }
        // The retirement ended before the larger planes were charged.
        Some(DrawingPath::Gpu) => eprintln!("{test}: the replaced planes had retired already"),
        None => panic!("a frame was drawn"),
    }
    settle(&pipeline);
    for frame in 0..5 {
        paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(ID, Some(larger.clone())),
        );
        let seen = diagnostics(&pipeline, ID);
        assert_eq!(
            (seen.drawn_path, seen.gpu_fallback),
            (Some(DrawingPath::Gpu), None),
            "frame {frame} once the replaced planes retired"
        );
        assert_eq!(seen.gpu_preview_in_use_bytes, SLOT_BYTES + link + large);
        assert!(seen.gpu_preview_peak_bytes <= budget);
    }
    assert_eq!(
        pipeline.figures.retirement_pending.load(Ordering::Acquire),
        0,
        "nothing more retires"
    );
}

/// A pass's pipeline is its kernel and its shape: two passes of one kernel at different word
/// offsets share one, and a second plan whose words sit at other offsets compiles none, yet draws
/// what its own words ask.
#[test]
fn pass_pipelines_depend_on_their_kernel_and_shape_alone() {
    let test = "pass_pipelines_depend_on_their_kernel_and_shape_alone";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, values) = boundary_values();
    // Two means of plane 0, each at its own words, then the lanes; the apply shows the first.
    let shaped = |lead: usize| {
        let mut spatial = test_spatial();
        let mut words = vec![0u32; lead];
        words.extend([RADIUS, RADIUS + 2, SCALE.to_bits()]);
        spatial.program.words = words;
        spatial.planes.insert(
            2,
            GpuPlane {
                format: PlaneFormat::Quad,
                size: PlaneSize::Reduced(1),
            },
        );
        let lead = lead as u32;
        spatial.passes = vec![
            GpuPass {
                kernel: Cow::Borrowed("lf_test_copy"),
                inputs: vec![],
                output: 0,
                words: lead,
                source: 0,
                shape: PassShape::Texels { span: [1, 1] },
                unit: 0,
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_mean_x"),
                inputs: vec![0],
                output: 1,
                words: lead,
                source: 0,
                shape: PassShape::Texels { span: [1, 1] },
                unit: 0,
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_mean_x"),
                inputs: vec![0],
                output: 2,
                words: lead + 1,
                source: 0,
                shape: PassShape::Texels { span: [1, 1] },
                unit: 0,
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_lanes"),
                inputs: vec![],
                output: 3,
                words: lead,
                source: 0,
                shape: PassShape::Workgroup,
                unit: 0,
            },
        ];
        spatial.applies[0].planes = vec![1, 3];
        spatial.applies[0].words = lead + 2;
        GpuPlan {
            boundary: boundary.clone(),
            texels: TexelMap::IDENTITY,
            steps: vec![
                GpuStep::colour(scale(0.5)),
                GpuStep::Spatial(Box::new(spatial)),
            ],
            region: None,
        }
    };
    let created = |pipeline: &PhotoPipeline| {
        pipeline
            .gpu
            .support
            .as_ref()
            .expect("a supported stage")
            .passes
            .created()
    };
    let expected = expected_codes(&values);
    for (lead, wanted) in [(0, 3), (5, 3)] {
        let drawn = paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(ID, Some(shaped(lead))),
        );
        assert_eq!(
            diagnostics(&pipeline, ID).drawn_path,
            Some(DrawingPath::Gpu),
            "words from {lead}"
        );
        // Four passes, three modules: the copy, the mean and the lanes.
        assert_eq!(created(&pipeline), wanted, "words from {lead}");
        for (pixel, [r, g, b]) in drawn.chunks_exact(4).zip(&expected) {
            for (drawn, wanted) in [(pixel[2], *r), (pixel[1], *g), (pixel[0], *b)] {
                assert!(drawn.abs_diff(wanted) <= 1, "words from {lead}");
            }
        }
    }
    // Each plan is a chain of two links: the colour step's, which the two share, and the spatial
    // step's, each its own frame pipeline over the shared passes.
    assert_eq!(pipeline.figures.preview.compiles.load(Ordering::Relaxed), 3);
    assert_eq!(
        pipeline.gpu.support.as_ref().unwrap().passes.len(),
        3,
        "the stage keeps each module once"
    );
}

/// A tick runs only the passes whose words, upstream or inputs changed since the planes the
/// applies read were written: a change to the apply's word alone encodes no compute pass and
/// still draws what it asks, while a change to the passes' words, to a step before, or to the
/// boundary runs them again.
#[test]
fn a_change_to_an_apply_alone_runs_no_pass() {
    let test = "a_change_to_an_apply_alone_runs_no_pass";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, values) = boundary_values();
    let shaped = |boundary: &GpuBoundary, factor: f32, radius: u32, scale_by: f32| {
        let mut spatial = test_spatial();
        spatial.program.words = vec![radius, scale_by.to_bits()];
        GpuPlan {
            boundary: boundary.clone(),
            texels: TexelMap::IDENTITY,
            steps: vec![
                GpuStep::colour(scale(factor)),
                GpuStep::Spatial(Box::new(spatial)),
            ],
            region: None,
        }
    };
    let dispatched = |pipeline: &PhotoPipeline| {
        pipeline.surfaces[&ID]
            .gpu
            .as_ref()
            .and_then(|slot| slot.spatial.as_ref())
            .expect("a spatial slot")
            .dispatched
    };
    let other = GpuBoundary::from_linear(
        crate::photo_surface::BoundaryFormat::Half,
        SIDE,
        SIDE,
        boundary.version() + 1,
        values.iter().map(|[r, g, b]| [*r, *g, *b, 1.0]),
    )
    .unwrap();
    // Each tick: its plan, and the passes it runs.
    let ticks = [
        ("the first", shaped(&boundary, 0.5, RADIUS, SCALE), 3),
        ("the apply's word", shaped(&boundary, 0.5, RADIUS, 0.25), 0),
        (
            "the apply's word again",
            shaped(&boundary, 0.5, RADIUS, 0.75),
            0,
        ),
        ("the passes' word", shaped(&boundary, 0.5, 1, 0.75), 3),
        ("the step before", shaped(&boundary, 0.25, 1, 0.75), 3),
        ("the boundary", shaped(&other, 0.25, 1, 0.75), 3),
        ("the apply's word last", shaped(&other, 0.25, 1, SCALE), 0),
    ];
    let mut before = 0;
    for (change, plan, runs) in ticks {
        let GpuStep::Colour { program, .. } = &plan.steps[0] else {
            unreachable!()
        };
        let factor = f32::from_bits(program.words[0]);
        let GpuStep::Spatial(spatial) = &plan.steps[1] else {
            unreachable!()
        };
        let (radius, scale_by) = (
            spatial.program.words[0],
            f32::from_bits(spatial.program.words[1]),
        );
        let drawn = paint(&device, &queue, &mut pipeline, &primitive(ID, Some(plan)));
        assert_eq!(
            diagnostics(&pipeline, ID).drawn_path,
            Some(DrawingPath::Gpu),
            "{change}"
        );
        let now = dispatched(&pipeline);
        assert_eq!(now - before, runs, "{change}");
        // The figure evidence reports is the slot's own.
        assert_eq!(
            diagnostics(&pipeline, ID).gpu_preview_spatial_passes,
            now,
            "{change}"
        );
        before = now;
        let expected = expected_codes_with(&values, factor, radius, scale_by);
        for (index, (pixel, [r, g, b])) in drawn.chunks_exact(4).zip(&expected).enumerate() {
            for (drawn, wanted) in [(pixel[2], *r), (pixel[1], *g), (pixel[0], *b)] {
                assert!(
                    drawn.abs_diff(wanted) <= 1,
                    "{change}: texel ({}, {}): {drawn} against {wanted}",
                    index as u32 % SIDE,
                    index as u32 / SIDE
                );
            }
        }
        eprintln!("{test}: {change}: {runs} passes");
    }
}

/// Chained spatial steps share scratch textures: a later step's plane no apply reads takes an
/// earlier step's scratch texture of the same format and extent, while every plane an apply reads
/// keeps its own, and the charge is the textures'.
#[test]
fn chained_spatial_steps_share_their_scratch_textures() {
    let steps = vec![
        GpuStep::Spatial(Box::new(test_spatial())),
        GpuStep::colour(scale(0.5)),
        GpuStep::Spatial(Box::new(test_spatial())),
    ];
    let key = super::super::spatial::PlanesKey::of(&steps, (SIDE, SIDE), (0, 0)).expect("planes");
    // Plane 0 is the copy only the passes read; planes 1 and 2 the apply reads.
    assert_eq!(
        key.texture(2, 0),
        key.texture(0, 0),
        "the scratch copy is shared"
    );
    let applied = [
        key.texture(0, 1),
        key.texture(0, 2),
        key.texture(2, 1),
        key.texture(2, 2),
    ];
    for (index, texture) in applied.iter().enumerate() {
        assert!(
            !applied[..index].contains(texture),
            "an apply's plane keeps its own"
        );
        assert_ne!(*texture, key.texture(0, 0));
    }
    let single =
        super::super::spatial::PlanesKey::of(&steps[..1], (SIDE, SIDE), (0, 0)).expect("planes");
    let copy = 64 * 64 * 8;
    // Two steps' planes, less the copy they share; each step's three passes' parameters.
    assert_eq!(key.bytes(), 2 * single.bytes() - copy);
}
