//! The repair program on the GPU (`docs/design/corrections.md`, "Rendering (TASK-003)"), held to
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
                op(1, false, size, &[[0.2 * w, 0.3 * h]], 8.0, [40.0, 10.0], 50, 100),
                op(2, false, size, &[[0.6 * w, 0.7 * h]], 6.0, [-30.0, -20.0], 0, 100),
                op(3, false, size, &[[0.8 * w, 0.2 * h]], 10.0, [0.0, 50.0], 80, 70),
            ],
        ),
        (
            "a clone reading another's output",
            vec![
                op(1, false, size, &[[0.4 * w, 0.4 * h]], 12.0, [30.0, 0.0], 50, 100),
                op(2, false, size, &[[0.4 * w + 30.0, 0.4 * h]], 12.0, [0.0, 40.0], 50, 100),
                op(3, false, size, &[[0.4 * w, 0.4 * h + 5.0]], 8.0, [-0.3 * w, 0.0], 40, 100),
            ],
        ),
        (
            "a heal spot",
            vec![op(1, true, size, &[[0.5 * w, 0.5 * h]], 14.0, [36.0, -12.0], 50, 100)],
        ),
        (
            "a heal stroke from far",
            vec![op(
                1,
                true,
                size,
                &[[0.2 * w, 0.6 * h], [0.35 * w, 0.65 * h], [0.5 * w, 0.62 * h]],
                7.0,
                [0.3 * w, -0.4 * h],
                30,
                90,
            )],
        ),
        (
            "edges and clipped sources",
            vec![
                op(1, true, size, &[[3.0, 4.0]], 9.0, [w - 12.0, h - 10.0], 50, 100),
                op(2, false, size, &[[w - 5.0, h - 4.0]], 9.0, [12.0, 8.0], 0, 100),
                op(3, true, size, &[[w - 6.0, 0.5 * h]], 8.0, [10.0, 0.0], 50, 100),
            ],
        ),
        (
            "overlapping heals and clones",
            vec![
                op(5, true, size, &[[0.3 * w, 0.4 * h]], 12.0, [30.0, -8.0], 50, 100),
                op(2, false, size, &[[0.25 * w, 0.4 * h], [0.36 * w, 0.44 * h]], 6.0, [-20.0, 30.0], 0, 60),
                op(9, true, size, &[[0.3 * w + 6.0, 0.4 * h]], 9.0, [0.0, 0.3 * h], 20, 100),
                op(7, false, size, &[[0.7 * w, 0.3 * h]], 2.0, [3.0, 0.0], 50, 100),
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
    let request = GpuPlanRequest::exact(0, Stage {
        width: size.0,
        height: size.1,
    })
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
    assert!(plan.spatial.first().is_some_and(|step| step.repair.is_some()));
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
        gpu.iter().all(|texel| texel[..3].iter().all(|v| v.is_finite())),
        "a finite frame"
    );
    let program = codes(gpu.iter().map(|texel| [texel[0], texel[1], texel[2]]));
    compare(width, height, &program, &rgb(&cpu.rgba))
}

/// Every case, alone and before Basic and Detail, on both paths at two sizes, against the CPU
/// reference within the spatial class.
#[test]
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
                    vec![Layer::new(BASIC_EFFECT, json!({"exposure": 0.6, "contrast": 25}))],
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
                    ("byte", byte_case(&qualifier, &registry, &stack, size, &pixels)),
                    ("linear", linear_case(&qualifier, &registry, &stack, size, &pixels)),
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
    assert!(missed.is_empty(), "cases missing the spatial limits: {missed:?}");
}
