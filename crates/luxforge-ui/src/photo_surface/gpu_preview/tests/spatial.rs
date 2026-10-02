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
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_mean_x"),
                inputs: vec![0],
                output: 1,
                words: 0,
                source: 0,
                shape: PassShape::Texels { span: [1, 1] },
            },
            GpuPass {
                kernel: Cow::Borrowed("lf_test_lanes"),
                inputs: vec![],
                output: 2,
                words: 0,
                source: 0,
                shape: PassShape::Workgroup,
            },
        ],
        applies: vec![GpuApply {
            function: Cow::Borrowed("lf_test_show"),
            planes: vec![1, 2],
            words: 1,
        }],
        clamps: true,
        mask: None,
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
    let boundary = GpuBoundary::from_linear(1, 1, 1, [[0.0; 4]]).unwrap();
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
    let side = SIDE as i64;
    let input = |x: i64, y: i64, channel: usize| {
        let x = x.clamp(0, side - 1);
        let value = values[(y * side + x) as usize][channel];
        // The pass held the copy as a half float.
        let halved = f64::from((value * 0.5).clamp(0.0, 1.0));
        f64::from(half::f16::from_f64(halved).to_f32())
    };
    (0..side * side)
        .map(|index| {
            let (x, y) = (index % side, index / side);
            let mean = |channel| {
                (-(RADIUS as i64)..=RADIUS as i64)
                    .map(|dx| input(x + dx, y, channel))
                    .sum::<f64>()
                    / f64::from(2 * RADIUS + 1)
            };
            [
                srgb::code(mean(0).clamp(0.0, 1.0)),
                srgb::code(mean(1).clamp(0.0, 1.0)),
                srgb::code(0.5 * f64::from(SCALE)),
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
    let planes = 64 * 64 * 8 + 64 * 64 * 16 + 16;
    paint(
        &device,
        &queue,
        &mut pipeline,
        &primitive(ID, Some(plan.clone())),
    );
    let seen = diagnostics(&pipeline, ID);
    assert_eq!(seen.drawn_path, Some(DrawingPath::Gpu));
    assert_eq!(seen.gpu_preview_in_use_bytes, SLOT_BYTES + planes);
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
