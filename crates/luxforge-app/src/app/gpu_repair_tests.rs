//! The repair program on the GPU (`docs/design/corrections-numerics.md`, "GPU form"), held to the
//! core's CPU units on this host's device, and its tick measured.
//!
//! - The program and a group's description pass the surface's own spatial convention.
//! - **Against the CPU**, on synthetic frames (flat, ramped, textured, a hard edge and a developed
//!   RAW's continuous range): spots and strokes of Clone and Heal, near, far and out-of-frame
//!   sources, the frame's edge, a source overlapping its destination, and two groups chained where
//!   the second samples the first's output. Each frame is judged by the spatial class's limits on
//!   the 8-bit codes and by its largest linear difference.
//! - **The tick** (ignored, a measurement): a Heal stroke growing tick by tick over a 24 MP and a
//!   60 MP boundary and over a Fit-sized one, through [`Qualifier::time_throughput`].
//!
//! A test with no adapter prints that it was skipped and asserts nothing: the skip is the report,
//! and `cargo test` counting it as passed does not make it GPU evidence.
use super::gpu_qualification::{Stream, codes, figures, worst};
use luxforge_core::GpuPlaneSize as PlaneExtent;
use luxforge_core::qualification::repair::{self as cpu, Operation, RepairGpu};
use luxforge_gpu::{
    BoundaryFormat, GpuApply, GpuPass, GpuPlan, GpuPlane, GpuProgram, GpuSpatial, GpuStep,
    PassShape, PlaneFormat, PlaneSize, TexelMap, qualification::boundary_as, validate_step,
};
use luxforge_reference::preview_error::{self, Class, Rgb8, Statistics};
use std::{borrow::Cow, sync::Arc};

/// One group's description as the surface's spatial step.
fn step(gpu: &RepairGpu, clamps: bool) -> GpuStep {
    let program = GpuProgram {
        words: gpu.words.clone(),
        block: Arc::from(gpu.block.clone()),
        ..GpuProgram::new("lf_repair", cpu::program_source())
    };
    let planes = gpu
        .planes
        .iter()
        .map(|&(width, height)| GpuPlane {
            format: PlaneFormat::Quad,
            size: PlaneSize::Extent(PlaneExtent::Fixed { width, height }),
        })
        .collect();
    let passes = gpu
        .passes
        .iter()
        .map(|pass| GpuPass {
            kernel: Cow::Borrowed(pass.kernel),
            inputs: pass.inputs.iter().map(|&i| i as u32).collect(),
            output: pass.output as u32,
            words: pass.words as u32,
            source: 0,
            reads_source: pass.reads_source,
            shape: PassShape::Texels { span: [1, 1] },
            unit: 0,
        })
        .collect();
    GpuStep::Spatial(Box::new(GpuSpatial {
        program,
        planes,
        passes,
        applies: vec![GpuApply {
            function: Cow::Borrowed("lf_repair_apply"),
            planes: vec![gpu.apply_plane as u32],
            words: gpu.apply_words as u32,
            identity: false,
        }],
        clamps,
        mask: None,
        halos: Vec::new(),
        contained: false,
        resident: Vec::new(),
        resident_key: 0,
        reads: None,
    }))
}

/// A plan of `groups` over a boundary of `pixels`, row by row.
fn plan(
    format: BoundaryFormat,
    (width, height): (u32, u32),
    pixels: &[[f32; 3]],
    groups: &[&RepairGpu],
) -> GpuPlan {
    GpuPlan {
        boundary: boundary_as(format, width, height, 1, pixels).expect("a boundary"),
        texels: TexelMap::IDENTITY,
        steps: groups.iter().map(|gpu| step(gpu, false)).collect(),
        region: None,
        lights: Vec::new(),
    }
}

fn planar(pixels: &[[f32; 3]]) -> Vec<f32> {
    let n = pixels.len();
    let mut planes = vec![0.0; 3 * n];
    for (at, p) in pixels.iter().enumerate() {
        planes[at] = p[0];
        planes[n + at] = p[1];
        planes[2 * n + at] = p[2];
    }
    planes
}

fn interleaved(planes: &[f32]) -> Vec<[f32; 3]> {
    let n = planes.len() / 3;
    (0..n)
        .map(|at| [planes[at], planes[n + at], planes[2 * n + at]])
        .collect()
}

/// The synthetic frames: flat, a ramp, a texture of noise over a ramp, a hard edge, and a developed
/// RAW's range (highlights past one, a little below zero).
fn frames(width: u32, height: u32) -> Vec<(&'static str, Vec<[f32; 3]>)> {
    let mut stream = Stream(0x7e9a);
    let noise: Vec<f32> = (0..width * height)
        .map(|_| (stream.unit() + stream.unit() - 1.0) as f32)
        .collect();
    let at = |x: u32, y: u32| noise[(y * width + x) as usize];
    let frame = |f: &dyn Fn(u32, u32) -> [f32; 3]| -> Vec<[f32; 3]> {
        (0..width * height)
            .map(|i| f(i % width, i / width))
            .collect()
    };
    let (w, h) = (width as f32, height as f32);
    vec![
        ("flat", frame(&|_, _| [0.18, 0.2, 0.22])),
        (
            "ramp",
            frame(&|x, y| [0.02 + 0.7 * x as f32 / w, 0.05 + 0.5 * y as f32 / h, 0.3]),
        ),
        (
            "texture",
            frame(&|x, y| {
                let ramp = 0.2 * (1.0 + 0.5 * x as f32 / w);
                let n = at(x, y);
                [
                    ramp * (1.0 + 0.5 * n),
                    ramp * 0.8 * (1.0 + 0.5 * n),
                    ramp * 1.2,
                ]
            }),
        ),
        (
            "edge",
            frame(&|x, _| {
                if x < width / 2 {
                    [0.01; 3]
                } else {
                    [0.9, 0.85, 0.8]
                }
            }),
        ),
        (
            "raw range",
            frame(&|x, y| {
                let hot = if (x as f32 - 0.62 * w).powi(2) + (y as f32 - 0.44 * h).powi(2)
                    < (0.12 * h).powi(2)
                {
                    3.5
                } else {
                    0.0
                };
                let n = 0.02 * at(x, y);
                [
                    -0.004 + 0.4 * x as f32 / w + hot + n,
                    0.3 + hot * 0.8 - n,
                    1.2 + n,
                ]
            }),
        ),
    ]
}

/// The operation cases on a `width x height` stage.
fn cases((width, height): (u32, u32)) -> Vec<(&'static str, Vec<Operation>)> {
    let size = (width, height);
    let (w, h) = (f64::from(width), f64::from(height));
    let heal = |points: &[[f64; 2]], r: f64, source: [f64; 2]| {
        Operation::pixels(true, size, points, r, source)
    };
    let clone = |points: &[[f64; 2]], r: f64, source: [f64; 2]| {
        Operation::pixels(false, size, points, r, source)
    };
    let stroke = [
        [0.1 * w, 0.2 * h],
        [0.5 * w, 0.25 * h],
        [0.45 * w, 0.8 * h],
        [0.9 * w, 0.7 * h],
    ];
    vec![
        (
            "heal spot",
            vec![heal(&[[0.5 * w, 0.5 * h]], 14.0, [40.0, 0.0])],
        ),
        (
            "heal hard spot",
            vec![Operation {
                feather: 0.0,
                ..heal(&[[0.5 * w, 0.5 * h]], 14.0, [0.0, 40.0])
            }],
        ),
        (
            "heal large spot",
            vec![heal(&[[0.45 * w, 0.5 * h]], 0.3 * h, [0.5 * w, 0.0])],
        ),
        (
            "heal tiny spot",
            vec![heal(&[[0.3 * w, 0.3 * h]], 1.0, [5.0, 3.0])],
        ),
        ("heal stroke", vec![heal(&stroke, 7.0, [4.0, 16.0])]),
        (
            "heal stroke brushed back and forth",
            vec![heal(
                &[
                    [0.15 * w, 0.5 * h],
                    [0.8 * w, 0.5 * h],
                    [0.25 * w, 0.52 * h],
                    [0.7 * w, 0.5 * h],
                ],
                9.0,
                [0.0, 30.0],
            )],
        ),
        (
            "heal at the frame edge",
            vec![heal(&[[4.0, 0.5 * h]], 20.0, [60.0, 0.0])],
        ),
        (
            "heal source off the frame",
            vec![heal(&[[0.2 * w, 0.4 * h]], 18.0, [-60.0, 0.0])],
        ),
        (
            "heal source over itself",
            vec![heal(&[[0.4 * w, 0.4 * h]], 22.0, [11.0, 4.0])],
        ),
        (
            "clone spot",
            vec![clone(&[[0.5 * w, 0.5 * h]], 14.0, [-30.0, 25.0])],
        ),
        ("clone stroke", vec![clone(&stroke, 5.0, [0.0, 9.0])]),
        (
            "a group",
            vec![
                heal(&[[0.2 * w, 0.3 * h]], 10.0, [0.0, 40.0]),
                clone(&[[0.8 * w, 0.3 * h]], 12.0, [0.0, 40.0]),
                heal(
                    &[[0.5 * w, 0.75 * h], [0.7 * w, 0.8 * h]],
                    8.0,
                    [0.0, -30.0],
                ),
            ],
        ),
    ]
}

/// The statistics of `gpu` against `cpu` over the whole frame, and the largest linear difference
/// relative to the larger of one and the CPU's value.
fn compare(width: u32, height: u32, gpu: &[[f32; 4]], cpu: &[[f32; 3]]) -> (Statistics, f64) {
    let gpu3: Vec<[f32; 3]> = gpu.iter().map(|p| [p[0], p[1], p[2]]).collect();
    let (a, b) = (codes(gpu3.iter().copied()), codes(cpu.iter().copied()));
    let statistics = preview_error::compare(
        Rgb8::new(width, height, &a).unwrap(),
        Rgb8::new(width, height, &b).unwrap(),
        [0, 0, width, height],
    )
    .unwrap();
    let largest = gpu3
        .iter()
        .zip(cpu)
        .flat_map(|(g, c)| {
            (0..3).map(move |k| f64::from((g[k] - c[k]).abs()) / f64::from(c[k].abs()).max(1.0))
        })
        .fold(0.0, f64::max);
    (statistics, largest)
}

// ---- Without a device -------------------------------------------------------------------------

/// A group's description of Clone and Heal, and two chained groups, pass the surface's own
/// spatial convention: every kernel and the apply have the convention's signatures, no pass reads
/// more than four planes or its own output.
#[test]
fn gpu_repair_the_program_passes_the_surfaces_own_convention() {
    let size = (320, 240);
    for (name, ops) in cases(size) {
        let gpu = cpu::describe(size, &ops, None);
        assert!(gpu.passes.len() >= 2, "{name}");
        validate_step(&step(&gpu, false)).unwrap_or_else(|error| panic!("{name}: {error}"));
        validate_step(&step(&gpu, true)).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
}

// ---- On a device -------------------------------------------------------------------------------

/// Every case over every frame against the CPU units, on an `rgba32float` boundary (a developed
/// RAW's) and, for the in-range frames, a half-float one (a JPEG's), judged by the spatial limits.
#[test]
fn slow_gpu_repair_matches_the_cpu_units() {
    let test = "slow_gpu_repair_matches_the_cpu_units";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let (width, height) = (320, 240);
    let mut all = Vec::new();
    let mut largest = 0.0f64;
    for (frame, pixels) in frames(width, height) {
        for (name, ops) in cases((width, height)) {
            for format in [BoundaryFormat::Float, BoundaryFormat::Half] {
                if format == BoundaryFormat::Half && frame == "raw range" {
                    continue;
                }
                // The CPU reads what the boundary holds.
                let held: Vec<[f32; 3]> = match format {
                    BoundaryFormat::Float => pixels.clone(),
                    BoundaryFormat::Half => pixels
                        .iter()
                        .map(|p| p.map(luxforge_gpu::qualification::held))
                        .collect(),
                };
                let mut expected = planar(&held);
                cpu::cpu((width, height), &mut expected, &ops).expect("the CPU units");
                let expected = interleaved(&expected);
                let gpu = cpu::describe((width, height), &ops, None);
                let drawn = qualifier
                    .evaluate(&plan(format, (width, height), &held, &[&gpu]))
                    .expect("a readback");
                let non_finite = drawn
                    .iter()
                    .filter(|p| p[..3].iter().any(|v| !v.is_finite()))
                    .count();
                assert_eq!(non_finite, 0, "{frame} {name}");
                let (statistics, linear) = compare(width, height, &drawn, &expected);
                eprintln!(
                    "{frame} {name} {format:?}: {} | linear rel {linear:.2e}",
                    figures(&statistics)
                );
                assert!(
                    preview_error::verdict(&statistics, Class::Spatial).passed(),
                    "{frame} {name} {format:?} misses the spatial limits: {}",
                    figures(&statistics)
                );
                largest = largest.max(linear);
                all.push(statistics);
            }
        }
    }
    eprintln!(
        "{test}: worst {} | linear rel {largest:.2e}",
        figures(&worst(&all))
    );
}

/// Two groups chained: the second samples the first's output, as the CPU's stored order makes it.
#[test]
fn gpu_repair_chained_groups_sample_the_earlier_output() {
    let test = "gpu_repair_chained_groups_sample_the_earlier_output";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    let (width, height) = (320, 240);
    let size = (width, height);
    let (_, pixels) = frames(width, height).swap_remove(2);
    let first = vec![
        Operation::pixels(true, size, &[[100.0, 120.0]], 18.0, [80.0, 0.0]),
        Operation::pixels(false, size, &[[240.0, 60.0]], 14.0, [0.0, 70.0]),
    ];
    // Its source is the first group's Heal; its destination the Clone's.
    let second = vec![Operation::pixels(
        true,
        size,
        &[[230.0, 70.0], [260.0, 80.0]],
        12.0,
        [-140.0, 45.0],
    )];
    let mut expected = planar(&pixels);
    let all: Vec<Operation> = first.iter().chain(&second).cloned().collect();
    cpu::cpu(size, &mut expected, &all).expect("the CPU units");
    let expected = interleaved(&expected);
    let one = cpu::describe(size, &first, None);
    let two = cpu::describe(size, &second, None);
    let drawn = qualifier
        .evaluate(&plan(BoundaryFormat::Float, size, &pixels, &[&one, &two]))
        .expect("a readback");
    let (statistics, linear) = compare(width, height, &drawn, &expected);
    eprintln!("{test}: {} | linear rel {linear:.2e}", figures(&statistics));
    assert!(preview_error::verdict(&statistics, Class::Spatial).passed());
    // And it is not the frame the second group would draw over the input alone.
    let alone = qualifier
        .evaluate(&plan(BoundaryFormat::Float, size, &pixels, &[&two]))
        .expect("a readback");
    assert!(alone.iter().zip(&drawn).any(|(a, b)| a != b));
}

/// The GPU's tick for a Heal stroke being painted: each tick adds a segment, so the group's
/// description (operation table, segments, tiles) changes and every pass runs again; the planes
/// are fixed at the finished stroke's capacity. Over a whole 24 MP boundary (the worst case: every
/// pass and the frame pass over the whole stage), a 4096 x 2560 window of a 60 MP stage (a view at
/// 100%; the surface's textures stop at 8192 a side) and the 1716 x 1144 Fit stage of a 24 MP
/// photograph with the stroke at the same content size (a drag at Fit). Beside each, the frame
/// with nothing to repair, and Detail (luminance 40, colour 40, sharpening 50) over the same
/// boundary evaluated where the stroke's destination changed on each tick, which is the work
/// repair-before-Detail adds downstream of a repair tick. Prints the wall time a tick from the
/// first later submission to the last completion and the planes' bytes; asserts nothing about
/// time. Run on request, on a quiet host: `cargo test -p luxforge-app --release --bin luxforge
/// gpu_repair_tick -- --ignored --nocapture`.
#[test]
#[ignore = "a measurement, run on request"]
fn gpu_repair_tick_cost() {
    use luxforge_core::{DETAIL_EFFECT, GpuAnswer, GpuPlanRequest, Layer, ModuleRegistry, Recipe};
    let test = "gpu_repair_tick_cost";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let registry = ModuleRegistry::builtin();
    let detail = Recipe {
        layers: vec![Layer::new(
            DETAIL_EFFECT,
            serde_json::json!({"luminance": 40, "colour": 40, "sharpening": 50, "radius": 1.0}),
        )],
        ..Recipe::default()
    };
    for (name, stage, window, radius, step_px) in [
        (
            "24 MP, whole, r=40 px",
            (6000u32, 4000u32),
            (6000u32, 4000u32),
            40.0,
            60.0,
        ),
        (
            "24 MP, whole, r=150 px",
            (6000, 4000),
            (6000, 4000),
            150.0,
            90.0,
        ),
        (
            "60 MP, 4096x2560 at 100%, r=60 px",
            (9504, 6336),
            (4096, 2560),
            60.0,
            70.0,
        ),
        (
            "24 MP at Fit 1716x1144, r=11 px",
            (1716, 1144),
            (1716, 1144),
            11.4,
            17.0,
        ),
        (
            "24 MP, whole, a spot of r=H/4=1000 px",
            (6000, 4000),
            (6000, 4000),
            1000.0,
            0.0,
        ),
    ] {
        let (w, h) = (f64::from(window.0), f64::from(window.1));
        let ticks = 24;
        let path = |n: usize| -> Vec<[f64; 2]> {
            (0..=n)
                .map(|k| {
                    let t = k as f64;
                    if step_px == 0.0 {
                        [0.3 * w, 0.35 * h]
                    } else {
                        [0.1 * w + step_px * t, 0.4 * h + step_px * (t * 0.7).sin()]
                    }
                })
                .collect()
        };
        let op = |n: usize| {
            vec![Operation::pixels(
                true,
                stage,
                &path(n),
                radius,
                if step_px == 0.0 {
                    [2.2 * radius, 0.0]
                } else {
                    [0.0, 4.0 * radius]
                },
            )]
        };
        let capacity = cpu::tile_counts(stage, &op(ticks));
        let mut stream = Stream(5);
        let pixels: Vec<[f32; 3]> = (0..window.0 * window.1)
            .map(|_| {
                let v = 0.2 + 0.05 * (stream.unit() as f32 - 0.5);
                [v, v * 0.9, v * 1.1]
            })
            .collect();
        let sequence: Vec<(GpuPlan, Option<[u32; 4]>)> = (1..=ticks)
            .map(|n| {
                let gpu = cpu::describe(stage, &op(n), Some(&capacity));
                (plan(BoundaryFormat::Half, window, &pixels, &[&gpu]), None)
            })
            .collect();
        let last = cpu::describe(stage, &op(ticks), Some(&capacity));
        let nothing = cpu::describe(stage, &[], None);
        let baseline: Vec<(GpuPlan, Option<[u32; 4]>)> = (0..ticks)
            .map(|_| {
                (
                    plan(BoundaryFormat::Half, window, &pixels, &[&nothing]),
                    None,
                )
            })
            .collect();
        let (encode, tick) = qualifier.time_throughput(&sequence).expect("the ticks");
        let (_, frame) = qualifier.time_throughput(&baseline).expect("the frames");
        // Detail where each tick's destination is: the stroke's box grown by the radius and the
        // ring, which a Heal tick changes whole.
        let request = GpuPlanRequest::exact(
            0,
            luxforge_core::Stage {
                width: window.0,
                height: window.1,
            },
        );
        let GpuAnswer::Plan(planned) =
            luxforge_core::gpu_plan(&registry, &detail, request).expect("Detail plans")
        else {
            panic!("Detail draws on the GPU");
        };
        let boundary =
            boundary_as(BoundaryFormat::Half, window.0, window.1, 1, &pixels).expect("a boundary");
        let converted = super::gpu_plan::surface_plan(&planned, boundary).expect("a runnable plan");
        let mut changed = 0.0;
        let downstream: Vec<(GpuPlan, Option<[u32; 4]>)> = (1..=ticks)
            .map(|n| {
                let points = path(n);
                let reach = radius + 2.0;
                let x0 = points.iter().map(|p| p[0]).fold(f64::INFINITY, f64::min) - reach;
                let x1 = points.iter().map(|p| p[0]).fold(0.0, f64::max) + reach;
                let y0 = points.iter().map(|p| p[1]).fold(f64::INFINITY, f64::min) - reach;
                let y1 = points.iter().map(|p| p[1]).fold(0.0, f64::max) + reach;
                let rect = [
                    x0.max(0.0) as u32,
                    y0.max(0.0) as u32,
                    (x1 as u32).min(window.0),
                    (y1 as u32).min(window.1),
                ];
                changed += f64::from((rect[2] - rect[0]) * (rect[3] - rect[1]))
                    / f64::from(window.0 * window.1);
                (converted.clone(), (n > 1).then_some(rect))
            })
            .collect();
        let (_, detail_tick) = qualifier
            .time_throughput(&downstream)
            .expect("Detail's ticks");
        let whole: Vec<(GpuPlan, Option<[u32; 4]>)> =
            (0..ticks).map(|_| (converted.clone(), None)).collect();
        let (_, detail_whole) = qualifier.time_throughput(&whole).expect("Detail whole");
        eprintln!(
            "{name}: {} passes, {} levels, tiles {:?}, planes {:.1} MiB | repair tick {:.2} ms \
             (encode {:.2} ms), the frame with nothing to repair {:.2} ms | Detail over the \
             changed destination ({:.1}% of the boundary on average) {:.2} ms, Detail whole \
             {:.2} ms",
            last.passes.len(),
            capacity.len(),
            capacity,
            last.plane_bytes() as f64 / (1024.0 * 1024.0),
            tick.as_secs_f64() * 1e3,
            encode.as_secs_f64() * 1e3,
            frame.as_secs_f64() * 1e3,
            100.0 * changed / ticks as f64,
            detail_tick.as_secs_f64() * 1e3,
            detail_whole.as_secs_f64() * 1e3,
        );
    }
}
