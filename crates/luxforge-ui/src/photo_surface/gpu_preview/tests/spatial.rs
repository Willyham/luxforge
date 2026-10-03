//! The spatial step's own tests: the spatial convention checked on a program alone, the modules the
//! surface assembles for its passes and frame, and, on a headless device, passes that fill planes
//! before the frame's pass reads them, charged to the budget and released with the slot. Last, a
//! chain's planes laid out as each link's kept textures and one pool of scratch textures, and the
//! chain's charge, without a device.
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
                reads_source: true,
                shape: PassShape::Texels { span: [1, 1] },
                unit: 0,
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_mean_x"),
                inputs: vec![0],
                output: 1,
                words: 0,
                source: 0,
                reads_source: false,
                shape: PassShape::Texels { span: [1, 1] },
                unit: 0,
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_lanes"),
                inputs: vec![],
                output: 2,
                words: 0,
                source: 0,
                reads_source: false,
                shape: PassShape::Workgroup,
                unit: 0,
            },
        ],
        applies: vec![GpuApply {
            function: Cow::Borrowed("lf_test_show"),
            planes: vec![1, 2],
            words: 1,
            identity: false,
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
        "an apply run by a pass that reads only planes",
        &|spatial| spatial.passes[1].source = 1,
        "reads only planes",
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
/// then its output. A pass that reads its unit's input runs the colour steps before the spatial one
/// in its source; one that reads only planes holds neither their statements nor their programs, so
/// its module is the same whatever steps come before its own. Every module validates with the naga
/// wgpu uses.
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
    let GpuStep::Spatial(spatial) = &plan.steps[1] else {
        unreachable!()
    };
    // The same step after other colour steps, and after none.
    let others = [
        vec![
            GpuStep::colour(identity()),
            GpuStep::colour(scale(0.5)),
            plan.steps[1].clone(),
        ],
        vec![plan.steps[1].clone()],
    ];
    for (pass, described) in spatial.passes.iter().enumerate() {
        let (module, slots): (String, Slots) =
            pass_module(&plan.steps, 1, described).expect("a pass's module");
        validate(&module).unwrap_or_else(|error| panic!("pass {pass}: {error}"));
        let reads = described.reads_source;
        assert_eq!(
            module.contains("rgb = scale(rgb"),
            reads,
            "pass {pass} runs the colour step"
        );
        assert_eq!(
            module.contains("rgb = clamp(rgb"),
            reads,
            "pass {pass} clamps its input"
        );
        assert_eq!(
            module.contains("fn scale("),
            reads,
            "pass {pass} holds the colour step's program"
        );
        assert_eq!(slots.len(), described.inputs.len());
        for steps in &others {
            let (other, _) =
                pass_module(steps, steps.len() - 1, described).expect("a pass's module");
            assert_eq!(other == module, !reads, "pass {pass} after other steps");
        }
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

/// An identity apply's planes need not be current, and they are in no key of what reads through
/// it: a tick runs none of the passes only they need, even as their words move, while the pass
/// writing a plane both units' applies read, and a later unit's passes, run as they would. Once
/// the apply is not the identity, every pass its planes need runs, and the later unit's again,
/// whose input is then the apply's output. A return to the identity reruns only the later unit's
/// passes, and a return from it, its planes still holding what they did, only those again.
#[test]
fn an_identity_applys_planes_are_written_only_once_it_is_not() {
    use super::super::spatial::{PlanesKey, Schedule};
    let (boundary, _) = boundary_values();
    // Unit A copies its input and takes its mean, and both units' applies read the lanes; unit B
    // copies its input through A's apply and takes its mean. Each pass has words of its own.
    let ticks = |radius: u32, identity: bool| {
        let mut spatial = test_spatial();
        spatial.program.words = vec![radius, SCALE.to_bits(), RADIUS, 0, 0];
        spatial
            .planes
            .extend([spatial.planes[0], spatial.planes[1]]);
        let [copy, mean, lanes] = [0, 1, 2].map(|pass| spatial.passes[pass].clone());
        let pass = |kernel: &GpuPass, inputs, output, words, source| GpuPass {
            inputs,
            output,
            words,
            source,
            ..kernel.clone()
        };
        spatial.passes = vec![
            pass(&copy, vec![], 0, 0, 0),
            pass(&mean, vec![0], 1, 0, 0),
            pass(&lanes, vec![], 2, 3, 0),
            pass(&copy, vec![], 3, 4, 1),
            pass(&mean, vec![3], 4, 2, 0),
        ];
        let apply = |planes: Vec<u32>, identity| GpuApply {
            function: Cow::Borrowed("lf_test_show"),
            planes,
            words: 1,
            identity,
        };
        spatial.applies = vec![apply(vec![1, 2], identity), apply(vec![4, 2], false)];
        validate_step(&GpuStep::Spatial(Box::new(spatial.clone()))).unwrap();
        GpuPlan {
            boundary: boundary.clone(),
            texels: TexelMap::IDENTITY,
            steps: vec![GpuStep::Spatial(Box::new(spatial))],
            region: None,
        }
    };
    let key = PlanesKey::of(&ticks(RADIUS, true).steps, (SIDE, SIDE), (0, 0)).expect("planes");
    let mut schedule = Schedule::default();
    let (mut words, mut blocks) = (Vec::new(), Vec::new());
    for (tick, plan, runs) in [
        (
            "A the identity",
            ticks(RADIUS, true),
            [false, false, true, true, true],
        ),
        ("A's words move", ticks(1, true), [false; 5]),
        (
            "A not the identity",
            ticks(1, false),
            [true, true, false, true, true],
        ),
        (
            "A the identity again",
            ticks(1, true),
            [false, false, false, true, true],
        ),
        (
            "A back, its planes current",
            ticks(1, false),
            [false, false, false, true, true],
        ),
    ] {
        super::super::pack(&plan, &mut words, &mut blocks);
        let ran = schedule.run(&plan.steps, &words, &blocks, plan.boundary.version(), &key);
        assert_eq!(ran, runs, "{tick}");
    }
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
/// what its own words ask. A plan whose colour steps before the spatial step differ compiles none
/// either, its spatial step's link reading what the link before wrote, and draws what it asks too.
#[test]
fn pass_pipelines_depend_on_their_kernel_and_shape_alone() {
    let test = "pass_pipelines_depend_on_their_kernel_and_shape_alone";
    let Some((device, queue)) = headless(test) else {
        return;
    };
    let mut pipeline = own_pipeline(&device, &queue);
    let (boundary, values) = boundary_values();
    // Two means of plane 0, each at its own words, then the lanes; the apply shows the first.
    let shaped = |lead: usize, before: &[GpuProgram]| {
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
                reads_source: true,
                shape: PassShape::Texels { span: [1, 1] },
                unit: 0,
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_mean_x"),
                inputs: vec![0],
                output: 1,
                words: lead,
                source: 0,
                reads_source: false,
                shape: PassShape::Texels { span: [1, 1] },
                unit: 0,
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_mean_x"),
                inputs: vec![0],
                output: 2,
                words: lead + 1,
                source: 0,
                reads_source: false,
                shape: PassShape::Texels { span: [1, 1] },
                unit: 0,
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_lanes"),
                inputs: vec![],
                output: 3,
                words: lead,
                source: 0,
                reads_source: false,
                shape: PassShape::Workgroup,
                unit: 0,
            },
        ];
        spatial.applies[0].planes = vec![1, 3];
        spatial.applies[0].words = lead + 2;
        let mut steps: Vec<GpuStep> = before.iter().cloned().map(GpuStep::colour).collect();
        steps.push(GpuStep::Spatial(Box::new(spatial)));
        GpuPlan {
            boundary: boundary.clone(),
            texels: TexelMap::IDENTITY,
            steps,
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
    // Each plan: what it changes, where its words start, the colour steps before its spatial
    // step, each halving as the first plan's does, and the pass pipelines created so far.
    let plans = [
        ("the first", 0, vec![scale(0.5)], 3),
        ("words from 5", 5, vec![scale(0.5)], 3),
        ("a step before", 0, vec![identity(), scale(0.5)], 3),
        ("another step before", 5, vec![swap(false), scale(0.5)], 3),
    ];
    for (change, lead, before, wanted) in &plans {
        let drawn = paint(
            &device,
            &queue,
            &mut pipeline,
            &primitive(ID, Some(shaped(*lead, before))),
        );
        assert_eq!(
            diagnostics(&pipeline, ID).drawn_path,
            Some(DrawingPath::Gpu),
            "{change}"
        );
        // Four passes, three modules: the copy, the mean and the lanes, none holding the steps
        // before, which their link reads as its boundary.
        assert_eq!(created(&pipeline), *wanted, "{change}");
        for (pixel, [r, g, b]) in drawn.chunks_exact(4).zip(&expected) {
            for (drawn, wanted) in [(pixel[2], *r), (pixel[1], *g), (pixel[0], *b)] {
                assert!(drawn.abs_diff(wanted) <= 1, "{change}");
            }
        }
    }
    // Each plan is a chain of two links: the colour steps', one sequence for each list of them, and
    // the spatial step's, one frame pipeline for each offset of its words over the shared passes.
    assert_eq!(pipeline.figures.preview.compiles.load(Ordering::Relaxed), 5);
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

// ---- The pool's layout, without a device ------------------------------------------------------

/// The boundary the layout tests cover, and the stage pixel of its first texel: a window whose
/// 4× blocks the boundary cuts on every side.
const LAID: (u32, u32) = (37, 21);
const LAID_AT: (u32, u32) = (5, 3);
/// Its texels, and the 4× blocks it reaches, blocks 1 to 10 across and 0 to 5 down.
const FULL: u64 = 37 * 21;
const QUARTER: u64 = 10 * 6;

fn plane(format: PlaneFormat, size: PlaneSize) -> GpuPlane {
    GpuPlane { format, size }
}

const R1: PlaneSize = PlaneSize::Reduced(1);
const R4: PlaneSize = PlaneSize::Reduced(4);
const ONE: PlaneSize = PlaneSize::Fixed {
    width: 1,
    height: 1,
};

/// A spatial step for the layout alone: `planes`, each with whether an apply reads it, a pass
/// writing each, and one apply reading every plane marked. Nothing runs it.
fn laid_out(planes: &[(PlaneFormat, PlaneSize, bool)]) -> GpuStep {
    let mut spatial = test_spatial();
    let pass = spatial.passes[1].clone();
    spatial.planes = planes
        .iter()
        .map(|&(format, size, _)| plane(format, size))
        .collect();
    spatial.passes = (0..planes.len() as u32)
        .map(|output| GpuPass {
            output,
            inputs: vec![(output + 1) % planes.len() as u32],
            ..pass.clone()
        })
        .collect();
    spatial.applies[0].planes = (0..planes.len() as u32)
        .filter(|&number| planes[number as usize].2)
        .collect();
    GpuStep::Spatial(Box::new(spatial))
}

/// A link of Presence's kind: a full-size colour, two full-size scalars, one held at half
/// precision, a 4× quad and a fixed estimate as scratch, and a half-precision scalar and a 4× pair
/// its apply reads.
fn shape_a() -> GpuStep {
    laid_out(&[
        (PlaneFormat::Colour, R1, false),
        (PlaneFormat::HalfScalar, R1, true),
        (PlaneFormat::Scalar, R1, false),
        (PlaneFormat::HalfScalar, R1, false),
        (PlaneFormat::Quad, R4, false),
        (PlaneFormat::Pair, R4, true),
        (PlaneFormat::Quad, ONE, false),
    ])
}

/// Another shape: two full-size colours, a pair and a half-precision pair, which a colour texture
/// would hold, and the fixed estimate as scratch; a full-size scalar and a 4× half-precision
/// scalar its apply reads.
fn shape_b() -> GpuStep {
    laid_out(&[
        (PlaneFormat::HalfPair, R1, false),
        (PlaneFormat::Colour, R1, false),
        (PlaneFormat::Colour, R1, false),
        (PlaneFormat::Scalar, R1, true),
        (PlaneFormat::Pair, R1, false),
        (PlaneFormat::Quad, ONE, false),
        (PlaneFormat::HalfScalar, R4, true),
    ])
}

fn class(format: PlaneFormat, size: PlaneSize) -> super::super::spatial::Class {
    super::super::spatial::Class { format, size }
}

/// Each shape's kept textures and parameter slices: seven passes each.
const KEPT_A: u64 = 4 * FULL + 8 * QUARTER + 7 * 256;
const KEPT_B: u64 = 4 * FULL + 4 * QUARTER + 7 * 256;
/// Each shape's scratch, each plane in a texture of its own.
const SCRATCH_A: u64 = 8 * FULL + 2 * 4 * FULL + 16 * QUARTER + 16;
const SCRATCH_B: u64 = 2 * 8 * FULL + 2 * 8 * FULL + 16;

/// Every link of `steps`' chain lays out its planes by the pool's rules, and the pool serves each
/// of them: a plane an apply reads is the link's own kept texture and never a pool texture, every
/// other plane is a pool texture of its own class, distinct scratch planes of one link take
/// distinct pool textures numbered from zero in plane order, and the pool holds, for each class,
/// exactly the most any one link needs. Answers the pool.
fn assert_served(steps: &[GpuStep]) -> super::super::spatial::PoolKey {
    use super::super::spatial::{Class, PlaneTexture, PlanesKey, PoolKey};
    let chain = super::super::chain::chain(steps);
    let links: Vec<&[GpuStep]> = chain
        .links
        .iter()
        .copied()
        .chain(std::iter::once(chain.last))
        .collect();
    let pool = PoolKey::of(links.iter().copied(), LAID, LAID_AT);
    let mut most: Vec<(Class, usize)> = Vec::new();
    for link in &links {
        let Some(key) = PlanesKey::of(link, LAID, LAID_AT) else {
            continue;
        };
        let mut kept = 0;
        let mut taken: Vec<(Class, usize)> = Vec::new();
        for (index, step) in link.iter().enumerate() {
            let GpuStep::Spatial(spatial) = step else {
                continue;
            };
            for (number, declared) in spatial.planes.iter().enumerate() {
                let read = spatial
                    .applies
                    .iter()
                    .any(|apply| apply.planes.contains(&(number as u32)));
                match key.location(index, number as u32).expect("a location") {
                    PlaneTexture::Kept(at) => {
                        assert!(read, "plane {number}: only a plane an apply reads is kept");
                        assert_eq!(at, kept, "plane {number}: kept textures in plane order");
                        kept += 1;
                    }
                    PlaneTexture::Pool(held, at) => {
                        assert!(!read, "plane {number}: no plane an apply reads is pooled");
                        assert_eq!(held, Class::of(*declared), "plane {number}: its own class");
                        let before = taken.iter().filter(|(other, _)| *other == held).count();
                        assert_eq!(at, before, "plane {number}: numbered in plane order");
                        assert!(!taken.contains(&(held, at)), "plane {number}: distinct");
                        let count = pool
                            .textures()
                            .iter()
                            .find(|(other, _)| *other == held)
                            .map_or(0, |(_, count)| *count);
                        assert!(at < count, "plane {number}: the pool holds its texture");
                        taken.push((held, at));
                    }
                }
            }
        }
        for &(held, count) in key.scratch() {
            match most.iter_mut().find(|(other, _)| *other == held) {
                Some((_, most)) => *most = (*most).max(count),
                None => most.push((held, count)),
            }
        }
    }
    most.sort();
    assert_eq!(
        pool.textures(),
        most,
        "the most any one link holds, per class"
    );
    pool
}

/// A link keeps in textures of its own exactly the planes its applies read, in plane order, and
/// numbers its scratch planes by class in plane order, reduced and fixed-size ones included:
/// `HalfScalar` with `Scalar`, in `r32float`, and a 4× or fixed plane in a class of its own size.
#[test]
fn a_link_keeps_the_planes_its_applies_read_and_numbers_its_scratch_by_class() {
    use super::super::spatial::{PlaneTexture, PlanesKey};
    let steps = [shape_a()];
    let key = PlanesKey::of(&steps, LAID, LAID_AT).expect("planes");
    let locations: Vec<PlaneTexture> = (0..7)
        .map(|number| key.location(0, number).expect("a location"))
        .collect();
    assert_eq!(
        locations,
        [
            PlaneTexture::Pool(class(PlaneFormat::Colour, R1), 0),
            PlaneTexture::Kept(0),
            PlaneTexture::Pool(class(PlaneFormat::Scalar, R1), 0),
            PlaneTexture::Pool(class(PlaneFormat::Scalar, R1), 1),
            PlaneTexture::Pool(class(PlaneFormat::Quad, R4), 0),
            PlaneTexture::Kept(1),
            PlaneTexture::Pool(class(PlaneFormat::Quad, ONE), 0),
        ]
    );
    assert_eq!(
        key.scratch(),
        [
            (class(PlaneFormat::Colour, R1), 1),
            (class(PlaneFormat::Scalar, R1), 2),
            (class(PlaneFormat::Quad, R4), 1),
            (class(PlaneFormat::Quad, ONE), 1),
        ]
    );
    assert_eq!(key.location(0, 7), None);
    assert_eq!(key.location(1, 0), None);
    assert_eq!(key.kept_bytes(), KEPT_A);
    assert_eq!(key.bytes(), KEPT_A + SCRATCH_A);
    // Alone, the link's pool is its own scratch.
    assert_eq!(assert_served(&steps).bytes(), SCRATCH_A);
}

/// A plan of one link lays out the textures its slot holds, a texture for every plane at its own
/// format and extent, which its passes write in, and its chain's charge is its planes' figure with
/// no intermediate. The test plan's spatial step after a colour step adds that link's intermediate
/// alone, and charges what the slot charges its planes and intermediate.
#[test]
fn a_single_link_lays_out_the_textures_it_held_and_charges_them() {
    use super::super::spatial::{PlanesKey, texture_formats, written_format};
    for (name, step) in [
        ("the test step", GpuStep::Spatial(Box::new(test_spatial()))),
        ("shape a", shape_a()),
        ("shape b", shape_b()),
    ] {
        let steps = [step, GpuStep::colour(scale(0.5))];
        let GpuStep::Spatial(spatial) = &steps[0] else {
            unreachable!()
        };
        let key = PlanesKey::of(&steps, LAID, LAID_AT).expect("planes");
        let textures: Vec<GpuPlane> = key.textures().collect();
        assert_eq!(textures.len(), spatial.planes.len(), "{name}");
        let mut seen = Vec::new();
        for (number, declared) in spatial.planes.iter().enumerate() {
            let texture = key.texture(0, number as u32);
            assert!(!seen.contains(&texture), "{name}: plane {number} alone");
            seen.push(texture);
            let held = textures[texture];
            assert_eq!(
                (held.format.texture(), held.extent(LAID_AT, LAID)),
                (declared.format.texture(), declared.extent(LAID_AT, LAID)),
                "{name}: plane {number}"
            );
            assert_eq!(
                written_format(&steps, 0, number as u32),
                Some(declared.format),
                "{name}: plane {number} is written in its own format"
            );
        }
        let own: Vec<PlaneFormat> = spatial.planes.iter().map(|plane| plane.format).collect();
        assert_eq!(texture_formats(&steps), [(0, own)], "{name}");
        assert_served(&steps);
        for format in [BoundaryFormat::Half, BoundaryFormat::Float] {
            let charge = chain_charge(&steps, LAID, LAID_AT, format);
            assert!(charge.intermediates.is_empty(), "{name}");
            assert_eq!(charge.kept, [key.kept_bytes()], "{name}");
            assert_eq!(charge.total(), key.bytes(), "{name}");
        }
    }
    // The test plan, as the slot test charges it: the colour link's intermediate, eight bytes a
    // texel of the half-float boundary, then the three planes and three passes' parameters.
    let (boundary, _) = boundary_values();
    let plan = spatial_plan(&boundary);
    let planes = 64 * 64 * 8 + 64 * 64 * 16 + 16 + 3 * 256;
    let charge = chain_charge(&plan.steps, (SIDE, SIDE), (0, 0), BoundaryFormat::Half);
    assert_eq!(charge.intermediates, [64 * 64 * 8]);
    assert_eq!(charge.kept, [0, 64 * 64 * 16 + 16 + 3 * 256]);
    assert_eq!(charge.pool, 64 * 64 * 8);
    assert_eq!(charge.total(), 64 * 64 * 8 + planes);
    let last = PlanesKey::of(&plan.steps[1..], (SIDE, SIDE), (0, 0)).expect("planes");
    assert_eq!(last.bytes(), planes);
}

/// Links of one shape share one set of scratch textures: two, and three with colour steps between
/// them, charge each link's kept planes and each intermediate before the last, and the pool once,
/// one link's scratch. Over a RAW's boundary an intermediate is sixteen bytes a texel.
#[test]
fn links_of_one_shape_share_one_links_scratch() {
    let half = 8 * FULL;
    for (name, steps, links) in [
        ("two links", vec![shape_a(), shape_a()], 2),
        (
            "three links with colour steps between",
            vec![
                shape_a(),
                GpuStep::colour(scale(0.5)),
                shape_a(),
                GpuStep::colour(scale(0.25)),
                shape_a(),
            ],
            3,
        ),
    ] {
        let pool = assert_served(&steps);
        assert_eq!(pool.bytes(), SCRATCH_A, "{name}");
        let charge = chain_charge(&steps, LAID, LAID_AT, BoundaryFormat::Half);
        assert_eq!(charge.intermediates, vec![half; links - 1], "{name}");
        assert_eq!(charge.kept, vec![KEPT_A; links], "{name}");
        assert_eq!(charge.pool, SCRATCH_A, "{name}");
        let total = (links as u64 - 1) * half + links as u64 * KEPT_A + SCRATCH_A;
        assert_eq!(charge.total(), total, "{name}");
        // Each link holding its own scratch, as the slot does, holds the rest again.
        let own = (links as u64 - 1) * half + links as u64 * (KEPT_A + SCRATCH_A);
        assert_eq!(
            own - charge.total(),
            (links as u64 - 1) * SCRATCH_A,
            "{name}"
        );
        let raw = chain_charge(&steps, LAID, LAID_AT, BoundaryFormat::Float);
        assert_eq!(raw.intermediates, vec![16 * FULL; links - 1], "{name}");
        assert_eq!(raw.total(), total + (links as u64 - 1) * 8 * FULL, "{name}");
    }
}

/// Links of different shapes take, for each class, as many pool textures as the link that needs
/// most, whatever order the links run in: a half-precision pair a colour texture would hold takes a
/// pair's texture all the same, beside the colours. A link's layout is its own steps' alone, so it
/// is the same in any chain; and the colour steps before the first spatial step are a link that
/// adds only its intermediate.
#[test]
fn the_pool_holds_for_each_class_the_most_any_one_link_holds() {
    use super::super::spatial::{PlanesKey, PoolKey};
    let pool = [
        (class(PlaneFormat::Colour, R1), 2),
        (class(PlaneFormat::Scalar, R1), 2),
        (class(PlaneFormat::Pair, R1), 2),
        (class(PlaneFormat::Quad, R4), 1),
        (class(PlaneFormat::Quad, ONE), 1),
    ];
    let pooled = 2 * 8 * FULL + 2 * 4 * FULL + 2 * 8 * FULL + 16 * QUARTER + 16;
    let half = 8 * FULL;
    for (name, steps, intermediates, kept) in [
        (
            "a then b",
            vec![shape_a(), shape_b()],
            1,
            vec![KEPT_A, KEPT_B],
        ),
        (
            "b then a",
            vec![shape_b(), shape_a()],
            1,
            vec![KEPT_B, KEPT_A],
        ),
        (
            "colour steps first, then a, a colour step and b",
            vec![
                GpuStep::colour(scale(0.5)),
                GpuStep::colour(identity()),
                shape_a(),
                GpuStep::colour(scale(0.25)),
                shape_b(),
            ],
            2,
            vec![0, KEPT_A, KEPT_B],
        ),
        (
            "a, b, then a again",
            vec![shape_a(), shape_b(), shape_a()],
            2,
            vec![KEPT_A, KEPT_B, KEPT_A],
        ),
    ] {
        let held = assert_served(&steps);
        assert_eq!(held.textures(), pool, "{name}");
        assert_eq!(held.bytes(), pooled, "{name}");
        let charge = chain_charge(&steps, LAID, LAID_AT, BoundaryFormat::Half);
        assert_eq!(charge.intermediates, vec![half; intermediates], "{name}");
        assert_eq!(charge.kept, kept, "{name}");
        assert_eq!(charge.pool, pooled, "{name}");
        assert_eq!(
            charge.total(),
            intermediates as u64 * half + kept.iter().sum::<u64>() + pooled,
            "{name}"
        );
    }
    // Each class's textures cover the window's blocks of their size.
    let held = PoolKey::of([&[shape_a()][..], &[shape_b()][..]], LAID, LAID_AT);
    assert_eq!(held.extent(class(PlaneFormat::Colour, R1)), (37, 21));
    assert_eq!(held.extent(class(PlaneFormat::Quad, R4)), (10, 6));
    assert_eq!(held.extent(class(PlaneFormat::Quad, ONE)), (1, 1));
    // Shape a's link lays out the same planes in either chain, alone or after another link.
    let alone = PlanesKey::of(&[shape_a()], LAID, LAID_AT);
    let after = [shape_b(), GpuStep::colour(scale(0.5)), shape_a()];
    let chain = super::super::chain::chain(&after);
    assert_eq!(PlanesKey::of(chain.last, LAID, LAID_AT), alone);
    let before = [shape_a(), GpuStep::colour(scale(0.5)), shape_b()];
    let chain = super::super::chain::chain(&before);
    assert_eq!(
        PlanesKey::of(&chain.links[0][..1], LAID, LAID_AT),
        alone,
        "the colour step after it adds no plane"
    );
}

/// The desktop's figure for a plan's spatial steps before its boundary exists holds every step's
/// planes in textures of their own, as each step's link does, with every pass's parameters.
#[test]
fn plane_bytes_holds_each_steps_planes_as_its_link_does() {
    let steps = [shape_a(), shape_b(), shape_a()];
    assert_eq!(
        super::super::spatial::plane_bytes(&steps, LAID, LAID_AT),
        2 * (KEPT_A + SCRATCH_A) + KEPT_B + SCRATCH_B
    );
}
