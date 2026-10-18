//! The repair program on the GPU (`docs/design/corrections.md`, "Rendering"), held to
//! the CPU reference on this host's device: Clone and Heal over overlapping, far and edge-clipped
//! sources, tiny and long strokes, hard and soft edges, on the byte and the linear path, alone and
//! before colour and Detail, each plan's frame judged by the spatial class's limits against the
//! reference renderer's frame of the same stack.
//!
//! A test with no adapter prints that it was skipped and asserts nothing: the skip is the report,
//! and `cargo test` counting it as passed does not make it GPU evidence.
use super::gpu_plan::surface_plan;
use super::gpu_qualification::{Stream, codes, figures, lit, worst};
use luxforge_core::{
    BASIC_EFFECT, BoundaryFormat, Cancel, DETAIL_EFFECT, GpuAnswer, GpuPlanRequest, Layer,
    LinearImage, LinearSettings, ModuleRegistry, PreviewSource, Recipe, RenderContext,
    RenderOptions, RenderSource, SnapshotId, SourceImage, Stage, gpu_plan,
    qualification::repair::{RepairTestModule, TEST_REPAIR_EFFECT},
    render,
};
use luxforge_gpu::qualification::{Qualifier, boundary, boundary_as};
use luxforge_reference::preview_error::{self, Class, Rgb8, Statistics};
use luxforge_test_macros::slow;
use serde_json::{Value, json};
use std::sync::Arc;

fn registry() -> ModuleRegistry {
    let mut registry = ModuleRegistry::builtin();
    registry
        .register(RepairTestModule::shared())
        .expect("the test repair module");
    registry
}

/// One operation in pixels of a `width × height` stage, put on the stored grid.
#[allow(clippy::too_many_arguments)]
pub(crate) fn op(
    id: u32,
    heal: bool,
    (width, height): (u32, u32),
    points: &[[f64; 2]],
    radius: f64,
    source: [f64; 2],
    feather: u8,
    opacity: u8,
) -> Value {
    let (w, h) = (f64::from(width), f64::from(height));
    let grid = |value: f64| (value * 16384.0).round() as i32;
    json!({
        "id": id,
        "kind": if heal { "heal" } else { "clone" },
        "points": points.iter().map(|[x, y]| [grid(x / w), grid(y / h)]).collect::<Vec<_>>(),
        "radius": grid(radius / h),
        "feather": feather,
        "opacity": opacity,
        "offset": [grid(source[0] / w), grid(source[1] / h)],
    })
}

pub(crate) fn repair(ops: Vec<Value>) -> Layer {
    Layer::new(TEST_REPAIR_EFFECT, json!({ "operations": ops }))
}

/// The cases: (name, operations) over a stage of `size`.
fn cases(size: (u32, u32)) -> Vec<(&'static str, Vec<Value>)> {
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    vec![
        (
            "independent clones",
            vec![
                op(
                    1,
                    false,
                    size,
                    &[[0.2 * w, 0.3 * h]],
                    8.0,
                    [40.0, 10.0],
                    50,
                    100,
                ),
                op(
                    2,
                    false,
                    size,
                    &[[0.6 * w, 0.7 * h]],
                    6.0,
                    [-30.0, -20.0],
                    0,
                    100,
                ),
                op(
                    3,
                    false,
                    size,
                    &[[0.8 * w, 0.2 * h]],
                    10.0,
                    [0.0, 50.0],
                    80,
                    70,
                ),
            ],
        ),
        (
            "a clone reading another's output",
            vec![
                op(
                    1,
                    false,
                    size,
                    &[[0.4 * w, 0.4 * h]],
                    12.0,
                    [30.0, 0.0],
                    50,
                    100,
                ),
                op(
                    2,
                    false,
                    size,
                    &[[0.4 * w + 30.0, 0.4 * h]],
                    12.0,
                    [0.0, 40.0],
                    50,
                    100,
                ),
                op(
                    3,
                    false,
                    size,
                    &[[0.4 * w, 0.4 * h + 5.0]],
                    8.0,
                    [-0.3 * w, 0.0],
                    40,
                    100,
                ),
            ],
        ),
        (
            "a heal spot",
            vec![op(
                1,
                true,
                size,
                &[[0.5 * w, 0.5 * h]],
                14.0,
                [36.0, -12.0],
                50,
                100,
            )],
        ),
        (
            "a heal stroke from far",
            vec![op(
                1,
                true,
                size,
                &[
                    [0.2 * w, 0.6 * h],
                    [0.35 * w, 0.65 * h],
                    [0.5 * w, 0.62 * h],
                ],
                7.0,
                [0.3 * w, -0.4 * h],
                30,
                90,
            )],
        ),
        (
            "edges and clipped sources",
            vec![
                op(
                    1,
                    true,
                    size,
                    &[[3.0, 4.0]],
                    9.0,
                    [w - 12.0, h - 10.0],
                    50,
                    100,
                ),
                op(
                    2,
                    false,
                    size,
                    &[[w - 5.0, h - 4.0]],
                    9.0,
                    [12.0, 8.0],
                    0,
                    100,
                ),
                op(
                    3,
                    true,
                    size,
                    &[[w - 6.0, 0.5 * h]],
                    8.0,
                    [10.0, 0.0],
                    50,
                    100,
                ),
            ],
        ),
        (
            "overlapping heals and clones",
            vec![
                op(
                    5,
                    true,
                    size,
                    &[[0.3 * w, 0.4 * h]],
                    12.0,
                    [30.0, -8.0],
                    50,
                    100,
                ),
                op(
                    2,
                    false,
                    size,
                    &[[0.25 * w, 0.4 * h], [0.36 * w, 0.44 * h]],
                    6.0,
                    [-20.0, 30.0],
                    0,
                    60,
                ),
                op(
                    9,
                    true,
                    size,
                    &[[0.3 * w + 6.0, 0.4 * h]],
                    9.0,
                    [0.0, 0.3 * h],
                    20,
                    100,
                ),
                op(
                    7,
                    false,
                    size,
                    &[[0.7 * w, 0.3 * h]],
                    2.0,
                    [3.0, 0.0],
                    50,
                    100,
                ),
            ],
        ),
    ]
}

/// A synthetic photograph in linear light: a sky over darker ground, a hard horizon, fine texture
/// and noise.
fn photograph(width: u32, height: u32, seed: u64) -> Vec<[f32; 3]> {
    let mut stream = Stream(seed);
    (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let base: [f32; 3] = if v < 0.45 {
                [0.55 + 0.2 * u, 0.62 + 0.15 * u, 0.75]
            } else {
                [0.12 + 0.1 * v, 0.16 + 0.05 * u, 0.08]
            };
            let texture = 0.05 * ((x as f32 * 1.3).sin() * (y as f32 * 0.9).cos());
            let noise = (stream.unit() as f32 - 0.5) * 0.04;
            base.map(|channel| (channel + texture + noise).max(0.0))
        })
        .collect()
}

fn compare(width: u32, height: u32, candidate: &[u8], reference: &[u8]) -> Statistics {
    let frame = |bytes| Rgb8::new(width, height, bytes).expect("a whole frame");
    preview_error::compare(frame(candidate), frame(reference), [0, 0, width, height])
        .expect("comparable frames")
}

fn rgb(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect()
}

fn plan_of(
    registry: &ModuleRegistry,
    stack: &Recipe,
    size: (u32, u32),
    linear: bool,
) -> luxforge_core::GpuPlan {
    let request = GpuPlanRequest::exact(
        0,
        Stage {
            width: size.0,
            height: size.1,
        },
    )
    .from_source()
    .qualifying();
    let request = if linear { request.linear() } else { request };
    match gpu_plan(registry, stack, request).expect("the stack compiles") {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("{reason}"),
    }
}

/// The byte path: an 8-bit source, the repair's input and output clamped where the CPU quantizes.
fn byte_case(
    qualifier: &Qualifier,
    registry: &ModuleRegistry,
    stack: &Recipe,
    size: (u32, u32),
    pixels: &[[f32; 3]],
) -> Statistics {
    let (width, height) = size;
    let rgba: Vec<u8> = pixels
        .iter()
        .flat_map(|rgb| {
            let [r, g, b] = rgb.map(|value| luxforge_reference::srgb::code(f64::from(value)));
            [r, g, b, 255]
        })
        .collect();
    let image = SourceImage {
        width,
        height,
        rgba: Arc::new(rgba.clone()),
        fingerprint: "sha256:gpu-repair-bytes".into(),
        orientation: 1,
        capture: Arc::default(),
    };
    let table = luxforge_core::colour::srgb::decode_table();
    let texels: Vec<[f32; 3]> = rgba
        .chunks_exact(4)
        .map(|pixel| [0, 1, 2].map(|c| table[usize::from(pixel[c])]))
        .collect();
    let cpu = render(
        registry,
        &image,
        stack,
        RenderOptions::exact(&Cancel::never()),
        &RenderContext::new(),
    )
    .and_then(|render| render.frame(SnapshotId::new()))
    .expect("the CPU frame");
    let plan = plan_of(registry, stack, size, false);
    assert!(
        plan.spatial
            .first()
            .is_some_and(|step| step.repair.is_some())
    );
    let converted = surface_plan(&plan, boundary(width, height, 1, &texels).unwrap()).unwrap();
    lit(qualifier, &PreviewSource::Jpeg(image.clone()), &converted).unwrap();
    let gpu = qualifier.evaluate(&converted).unwrap();
    let program = codes(gpu.iter().map(|texel| [texel[0], texel[1], texel[2]]));
    if stack.layers.len() == 1 {
        // The repair alone: the GPU frame is not the source's, so the step drew something.
        assert_ne!(program, rgb(&rgba), "the GPU frame shows the repair");
    }
    compare(width, height, &program, &rgb(&cpu.rgba))
}

/// The linear path: a continuous source held in `f32`, nothing clamped before the output.
fn linear_case(
    qualifier: &Qualifier,
    registry: &ModuleRegistry,
    stack: &Recipe,
    size: (u32, u32),
    pixels: &[[f32; 3]],
) -> Statistics {
    let (width, height) = size;
    let len = pixels.len();
    let mut planes = vec![0.0_f32; 3 * len];
    for (index, pixel) in pixels.iter().enumerate() {
        for channel in 0..3 {
            planes[channel * len + index] = pixel[channel];
        }
    }
    let image = LinearImage::with_fingerprint(width, height, planes, "sha256:gpu-repair")
        .expect("a linear image");
    let cpu = render(
        registry,
        RenderSource::Linear {
            image: &image,
            settings: LinearSettings::default(),
        },
        stack,
        RenderOptions::exact(&Cancel::never()),
        &RenderContext::new(),
    )
    .and_then(|render| render.frame(SnapshotId::new()))
    .expect("the CPU frame");
    let plan = plan_of(registry, stack, size, true);
    let held = boundary_as(BoundaryFormat::Float, width, height, 1, pixels).expect("a boundary");
    let converted = surface_plan(&plan, held).expect("a runnable plan");
    let preview = PreviewSource::Raw {
        image: image.clone(),
        settings: LinearSettings::default(),
    };
    lit(qualifier, &preview, &converted).expect("the plan's lights");
    let gpu = qualifier.evaluate(&converted).expect("a readback");
    assert!(
        gpu.iter()
            .all(|texel| texel[..3].iter().all(|v| v.is_finite())),
        "a finite frame"
    );
    let program = codes(gpu.iter().map(|texel| [texel[0], texel[1], texel[2]]));
    compare(width, height, &program, &rgb(&cpu.rgba))
}

/// Every case, alone and before Basic and Detail, on both paths at two sizes, against the CPU
/// reference within the spatial class.
#[test]
#[slow]
fn gpu_repair_plan_meets_the_spatial_limits() {
    let test = "gpu_repair_plan_meets_the_spatial_limits";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let registry = registry();
    let mut all = Vec::new();
    let mut missed = Vec::new();
    for size in [(320, 240), (960, 640)] {
        let pixels = photograph(size.0, size.1, u64::from(size.0));
        for (name, ops) in cases(size) {
            for (tail, after) in [
                ("", Vec::new()),
                (
                    " before Basic",
                    vec![Layer::new(
                        BASIC_EFFECT,
                        json!({"exposure": 0.6, "contrast": 25}),
                    )],
                ),
                (
                    " before Detail",
                    vec![Layer::new(DETAIL_EFFECT, json!({"sharpening": 60.0}))],
                ),
            ] {
                let mut layers = vec![repair(ops.clone())];
                layers.extend(after);
                let stack = Recipe {
                    layers,
                    ..Recipe::default()
                };
                for (path, measured) in [
                    (
                        "byte",
                        byte_case(&qualifier, &registry, &stack, size, &pixels),
                    ),
                    (
                        "linear",
                        linear_case(&qualifier, &registry, &stack, size, &pixels),
                    ),
                ] {
                    let label = format!("{name}{tail} on the {path} path at {}x{}", size.0, size.1);
                    let passed = preview_error::verdict(&measured, Class::Spatial).passed();
                    eprintln!(
                        "{label}: {}{}",
                        figures(&measured),
                        if passed { "" } else { " MISS" }
                    );
                    if !passed {
                        missed.push(label);
                    }
                    all.push(measured);
                }
            }
        }
    }
    eprintln!("{test}: worst of {}: {}", all.len(), figures(&worst(&all)));
    assert!(
        missed.is_empty(),
        "cases missing the spatial limits: {missed:?}"
    );
}

/// A Corrections gesture's ticks evaluated incrementally, as one slot draws them
/// (`GpuPlan::changes_since`): a Heal stroke painted a segment a tick over a Clone and a Heal
/// already held, then the Clone's source dragged, then its opacity, with Detail after the repair.
/// Each tick's change is the repaired destinations it touches, never anywhere, and every tick's
/// frame is the frame a whole evaluation of the same plan draws, bit for bit, with the poison on
/// too: Detail runs only over the change the repair hands on.
#[test]
#[slow]
fn gpu_repair_ticks_draw_what_whole_evaluations_draw() {
    let test = "gpu_repair_ticks_draw_what_whole_evaluations_draw";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let registry = registry();
    let size = (640u32, 480u32);
    let pixels = photograph(size.0, size.1, 5);
    let held_boundary = boundary(size.0, size.1, 1, &pixels).expect("a boundary");
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    let stroke: Vec<[f64; 2]> = (0..8)
        .map(|k| {
            [
                0.2 * w + 12.0 * f64::from(k),
                0.7 * h + 3.0 * f64::from(k % 3),
            ]
        })
        .collect();
    let stack = |painted: usize, source: f64, opacity: u8| {
        let mut ops = vec![
            op(
                1,
                false,
                size,
                &[[0.6 * w, 0.3 * h]],
                10.0,
                [source, 30.0],
                40,
                opacity,
            ),
            op(
                2,
                true,
                size,
                &[[0.75 * w, 0.6 * h]],
                12.0,
                [-50.0, 0.0],
                50,
                100,
            ),
        ];
        if painted > 0 {
            ops.push(op(
                3,
                true,
                size,
                &stroke[..painted],
                6.0,
                [0.0, -60.0],
                50,
                100,
            ));
        }
        Recipe {
            layers: vec![
                repair(ops),
                Layer::new(
                    DETAIL_EFFECT,
                    json!({"sharpening": 50.0, "luminance": 30.0}),
                ),
            ],
            ..Recipe::default()
        }
    };
    let mut order: Vec<(usize, f64, u8)> = (1..=stroke.len()).map(|k| (k, -40.0, 100)).collect();
    order.extend([(8, -36.0, 100), (8, -30.0, 100), (8, -30.0, 70)]);
    let plans: Vec<luxforge_core::GpuPlan> = order
        .iter()
        .map(|&(painted, source, opacity)| {
            plan_of(&registry, &stack(painted, source, opacity), size, false)
        })
        .collect();
    let mut shares = Vec::new();
    let ticks: Vec<_> = plans
        .iter()
        .enumerate()
        .map(|(number, plan)| {
            let inside = (number > 0).then(|| match plan.changes_since(&plans[number - 1]) {
                luxforge_core::GpuChange::Inside(rect) => {
                    shares.push(rect.pixels() as f64 / f64::from(size.0 * size.1));
                    [rect.x0, rect.y0, rect.x1(), rect.y1()]
                }
                luxforge_core::GpuChange::Nothing => [0; 4],
                luxforge_core::GpuChange::Anywhere => panic!("tick {number}: a change anywhere"),
            });
            let converted = surface_plan(plan, held_boundary.clone()).expect("a runnable plan");
            (converted, inside)
        })
        .collect();
    let whole: Vec<_> = ticks
        .iter()
        .map(|(plan, _)| qualifier.evaluate(plan).expect("a readback"))
        .collect();
    super::gpu_qualification::held_to_whole(&qualifier, &ticks, &whole, &|number| {
        format!(
            "tick {number} {:?} over {:?}",
            order[number], ticks[number].1
        )
    });
    let largest = shares.iter().copied().fold(0.0, f64::max);
    eprintln!(
        "{test}: {} ticks, each changing at most {:.1}% of the stage",
        shares.len(),
        100.0 * largest
    );
    assert!(
        largest < 0.25,
        "a tick changes about its corrections' own area"
    );
}

/// A hundred corrections as an agent filling a photograph places them, over a `size` stage: spots
/// and short strokes over the frame, a third of them Clones, each source a few radii away.
pub(crate) fn hundred(size: (u32, u32), seed: u64) -> Vec<Value> {
    let mut stream = Stream(seed);
    let (w, h) = (f64::from(size.0), f64::from(size.1));
    (1..=100u32)
        .map(|id| {
            let mut unit = || stream.unit();
            let (x, y) = (0.05 + 0.9 * unit(), 0.05 + 0.9 * unit());
            let radius = h * (0.006 + 0.01 * unit());
            let angle = std::f64::consts::TAU * unit();
            let reach = radius * (3.0 + 2.0 * unit());
            let points = if id % 4 == 0 {
                vec![[x * w, y * h], [x * w + 3.0 * radius, y * h + radius]]
            } else {
                vec![[x * w, y * h]]
            };
            op(
                id,
                id % 3 != 0,
                size,
                &points,
                radius,
                [reach * angle.cos(), reach * angle.sin()],
                50,
                100,
            )
        })
        .collect()
}

/// The limit: a hundred corrections drawn by the GPU in one bounded step, on both paths, within
/// the spatial class of the reference's frame.
#[test]
#[slow]
fn gpu_repair_a_hundred_corrections_meet_the_spatial_limits() {
    let test = "gpu_repair_a_hundred_corrections_meet_the_spatial_limits";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let registry = registry();
    let size = (1536u32, 1024u32);
    let pixels = photograph(size.0, size.1, 9);
    let stack = Recipe {
        layers: vec![repair(hundred(size, 3))],
        ..Recipe::default()
    };
    let plan = plan_of(&registry, &stack, size, false);
    let passes = plan.spatial[0].passes.len();
    let mut missed = Vec::new();
    for (path, measured) in [
        (
            "byte",
            byte_case(&qualifier, &registry, &stack, size, &pixels),
        ),
        (
            "linear",
            linear_case(&qualifier, &registry, &stack, size, &pixels),
        ),
    ] {
        let passed = preview_error::verdict(&measured, Class::Spatial).passed();
        eprintln!("{test}: {path}: {passes} passes: {}", figures(&measured));
        if !passed {
            missed.push(path);
        }
    }
    assert!(
        passes <= luxforge_core::REPAIR_PASSES_MAX,
        "{passes} passes"
    );
    assert!(missed.is_empty(), "{missed:?}");
}

/// Frozen patches (`docs/design/corrections.md`, "Frozen patches") drawn by the repair step on this
/// host's device against the reference's frame of the same stack: a patch alone (the step's apply),
/// a patch read by a later Clone (a chained pass from the step's input), and the tile families'
/// patches among Clone and Heal, alone and before Basic and Detail, on both paths. Coverage steps
/// through 0 to 1 and values pass both ends of the linear range.
#[test]
#[slow]
fn gpu_frozen_patches_meet_the_spatial_limits() {
    use luxforge_core::qualification::corrections::{QualBrush, QualPatch, QualStep, repair_stack};
    let test = "gpu_frozen_patches_meet_the_spatial_limits";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let size = (360u32, 240u32);
    let pixels = photograph(size.0, size.1, 17);
    let ramp = |seed: u32| -> Box<dyn Fn(u32, u32) -> ([f32; 3], f32)> {
        Box::new(move |x, y| {
            let value = |channel: u32| {
                ((x * 5 + y * 11 + channel * 37 + seed) % 256) as f32 / 255.0 * 1.5 - 0.25
            };
            (
                [value(0), value(1), value(2)],
                ((x + 2 * y + seed) % 5) as f32 / 4.0,
            )
        })
    };
    let alone = || {
        vec![QualStep::Patch(QualPatch {
            rect: [100, 60, 90, 70],
            opacity: 85,
            pixel: ramp(1),
        })]
    };
    let read = || {
        vec![
            QualStep::Patch(QualPatch {
                rect: [40, 40, 64, 48],
                opacity: 100,
                pixel: ramp(2),
            }),
            QualStep::Brush(QualBrush {
                heal: false,
                points: vec![[250.0 / 360.0, 150.0 / 240.0]],
                radius: 12.0 / 240.0,
                source: [70.0 / 360.0, 62.0 / 240.0],
                feather: 30,
                opacity: 100,
            }),
        ]
    };
    let mut missed = Vec::new();
    let mut all = Vec::new();
    let shapes: [(&str, &dyn Fn() -> Vec<QualStep>); 2] =
        [("a patch alone", &alone), ("a patch a Clone reads", &read)];
    for (name, steps) in shapes {
        for (tail, after) in [
            ("", Vec::new()),
            (
                " before Basic",
                vec![Layer::new(
                    BASIC_EFFECT,
                    json!({"exposure": 0.6, "contrast": 25}),
                )],
            ),
            (
                " before Detail",
                vec![Layer::new(DETAIL_EFFECT, json!({"sharpening": 60.0}))],
            ),
        ] {
            let stack = repair_stack(steps(), after);
            let plan = plan_of(&registry, &stack, size, false);
            assert!(
                !plan.spatial[0].resident.is_empty(),
                "{name}: the patch's words are resident in the step"
            );
            for (path, measured) in [
                (
                    "byte",
                    byte_case(&qualifier, &registry, &stack, size, &pixels),
                ),
                (
                    "linear",
                    linear_case(&qualifier, &registry, &stack, size, &pixels),
                ),
            ] {
                let label = format!("{name}{tail} on the {path} path");
                let passed = preview_error::verdict(&measured, Class::Spatial).passed();
                eprintln!(
                    "{label}: {}{}",
                    figures(&measured),
                    if passed { "" } else { " MISS" }
                );
                if !passed {
                    missed.push(label);
                }
                all.push(measured);
            }
        }
    }
    for (name, stack) in [
        (
            "patches among Clone and Heal",
            super::gpu_tiles_tests::patches(Vec::new()),
        ),
        (
            "patches among Clone and Heal before Detail",
            super::gpu_tiles_tests::patches(vec![Layer::new(
                DETAIL_EFFECT,
                json!({"sharpening": 60.0}),
            )]),
        ),
    ] {
        for (path, measured) in [
            (
                "byte",
                byte_case(&qualifier, &registry, &stack, size, &pixels),
            ),
            (
                "linear",
                linear_case(&qualifier, &registry, &stack, size, &pixels),
            ),
        ] {
            let label = format!("{name} on the {path} path");
            let passed = preview_error::verdict(&measured, Class::Spatial).passed();
            eprintln!(
                "{label}: {}{}",
                figures(&measured),
                if passed { "" } else { " MISS" }
            );
            if !passed {
                missed.push(label);
            }
            all.push(measured);
        }
    }
    eprintln!("{test}: worst of {}: {}", all.len(), figures(&worst(&all)));
    assert!(
        missed.is_empty(),
        "cases missing the spatial limits: {missed:?}"
    );
}

/// Frozen patches at a reduced stage (`docs/design/corrections.md`, "The composite at a reduced
/// stage"): a Fit frame's plan draws the repair stage smaller than the patches were placed, from
/// their box means, and is held to the CPU reference at that same stage — the layer compiled as
/// the host compiles a proxy and run over the same reduced frame — by the spatial class's limits
/// on the codes, with its largest linear difference reported. A patch alone, one a later Clone
/// reads, and overlapping patches, at a half, a third and two fifths of the content.
#[test]
#[slow]
fn gpu_frozen_patches_at_a_reduced_stage_meet_the_cpu_at_that_stage() {
    use luxforge_core::qualification::corrections::{
        QualBrush, QualPatch, QualStep, reduced, repair_stack,
    };
    let test = "gpu_frozen_patches_at_a_reduced_stage_meet_the_cpu_at_that_stage";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let full = (360u32, 240u32);
    let ramp = |seed: u32| -> Box<dyn Fn(u32, u32) -> ([f32; 3], f32)> {
        Box::new(move |x, y| {
            let value = |channel: u32| {
                ((x * 5 + y * 11 + channel * 37 + seed) % 256) as f32 / 255.0 * 1.5 - 0.25
            };
            (
                [value(0), value(1), value(2)],
                ((x + 2 * y + seed) % 5) as f32 / 4.0,
            )
        })
    };
    let patch = |rect: [u32; 4], opacity: u8, seed: u32| {
        QualStep::Patch(QualPatch {
            rect,
            opacity,
            pixel: ramp(seed),
        })
    };
    type Steps<'a> = Box<dyn Fn() -> Vec<QualStep> + 'a>;
    let shapes: [(&str, Steps<'_>); 3] = [
        (
            "a patch alone",
            Box::new(|| vec![patch([101, 61, 91, 71], 85, 1)]),
        ),
        (
            "a patch a Clone reads",
            Box::new(|| {
                vec![
                    patch([40, 40, 64, 48], 100, 2),
                    QualStep::Brush(QualBrush {
                        heal: false,
                        points: vec![[250.0 / 360.0, 150.0 / 240.0]],
                        radius: 12.0 / 240.0,
                        source: [70.0 / 360.0, 62.0 / 240.0],
                        feather: 30,
                        opacity: 100,
                    }),
                ]
            }),
        ),
        (
            "overlapping patches",
            Box::new(|| {
                vec![
                    patch([30, 30, 150, 100], 100, 3),
                    patch([97, 77, 133, 113], 60, 4),
                ]
            }),
        ),
    ];
    let mut missed = Vec::new();
    let mut all = Vec::new();
    let mut largest = 0.0f64;
    for (name, steps) in &shapes {
        let stack = repair_stack(steps(), Vec::new());
        for stage in [(180u32, 120u32), (120, 80), (144, 96)] {
            let pixels = photograph(stage.0, stage.1, 29);
            let cpu = reduced(&stack, stage, full, &pixels);
            let request = GpuPlanRequest::fit(
                0,
                Stage {
                    width: stage.0,
                    height: stage.1,
                },
                Stage {
                    width: full.0,
                    height: full.1,
                },
            )
            .from_source()
            .qualifying()
            .linear();
            let plan = match gpu_plan(&registry, &stack, request).expect("the stack compiles") {
                GpuAnswer::Plan(plan) => *plan,
                GpuAnswer::Fallback(reason) => panic!("{name} at {stage:?}: {reason}"),
            };
            assert!(
                !plan.spatial[0].resident.is_empty(),
                "{name}: the patch's means are resident in the step"
            );
            let held = boundary_as(BoundaryFormat::Float, stage.0, stage.1, 1, &pixels)
                .expect("a boundary");
            let converted = surface_plan(&plan, held).expect("a runnable plan");
            let gpu = qualifier.evaluate(&converted).expect("a readback");
            let linear = gpu
                .iter()
                .zip(&cpu)
                .flat_map(|(g, c)| (0..3).map(move |i| f64::from((g[i] - c[i]).abs())))
                .fold(0.0, f64::max);
            largest = largest.max(linear);
            let measured = compare(
                stage.0,
                stage.1,
                &codes(gpu.iter().map(|texel| [texel[0], texel[1], texel[2]])),
                &codes(cpu.iter().copied()),
            );
            let passed = preview_error::verdict(&measured, Class::Spatial).passed();
            let label = format!("{name} at {}x{}", stage.0, stage.1);
            eprintln!(
                "{label}: {} | largest linear {linear:.2e}{}",
                figures(&measured),
                if passed { "" } else { " MISS" }
            );
            if !passed {
                missed.push(label);
            }
            all.push(measured);
        }
    }
    eprintln!(
        "{test}: worst of {}: {} | largest linear {largest:.2e}",
        all.len(),
        figures(&worst(&all))
    );
    assert!(
        missed.is_empty(),
        "cases missing the spatial limits: {missed:?}"
    );
}
