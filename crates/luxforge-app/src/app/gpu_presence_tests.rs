//! The Presence programs on the GPU (`docs/design/gpu-preview.md`, "Spatial programs"), held to
//! their CPU filters and units on this host's device.
//!
//! - The core's spatial program passes the surface's own spatial convention, and a Presence plan
//!   converts into one spatial step.
//! - **Per filter**, on synthetic planes: each kernel of `presence.wgsl` run through the surface's
//!   own assembled passes against the CPU filter it transcribes (`luxforge_core::qualification`):
//!   the separable box mean and minimum, the self-guided and guided filters, the 4x and 16x block
//!   reductions, the bilinear upsample, the soft clip and the atmospheric light. The box means'
//!   running sums are measured at several run lengths.
//! - **Per unit**, on synthetic images: every Presence combination's plan against the CPU frame of
//!   the same stack, judged by the spatial limits, with the atmospheric light both stored and taken
//!   on the GPU.
//!
//! A test with no adapter prints that it was skipped and asserts nothing: the skip is the report,
//! and `cargo test` counting it as passed does not make it GPU evidence.
use super::gpu_plan::surface_plan;
use super::gpu_qualification::{Stream, codes, corpus_at_fit, figures, worst};
use luxforge_core::{
    Cancel, GPU_PROGRAMS, GpuAnswer, GpuEstimates, GpuPlanRequest, GpuProgramKind, Layer,
    LinearImage, LinearSettings, ModuleRegistry, PRESENCE_EFFECT, Recipe, RenderContext,
    RenderOptions, RenderSource, SnapshotId, SourceImage, Stage, gpu_plan, gpu_plan_with,
    qualification::presence as cpu, render,
};
use luxforge_reference::preview_error::{self, Class, Rgb8, Statistics};
use luxforge_ui::photo_surface::{
    GpuBoundary, GpuPlan, GpuProgram, GpuStep, TexelMap,
    gpu_preview::{
        GpuApply, GpuPass, GpuPlane, GpuSpatial, PassShape, PlaneFormat, PlaneSize,
        qualification::{Qualifier, boundary, held},
    },
    validate_step,
};
use serde_json::{Value, json};
use std::{borrow::Cow, sync::Arc};

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

fn presence() -> &'static luxforge_core::GpuProgram {
    GPU_PROGRAMS
        .iter()
        .find(|program| program.entry == "lf_presence")
        .expect("the Presence program ships")
}

fn recipe(payload: Value) -> Recipe {
    Recipe {
        layers: vec![Layer::new(PRESENCE_EFFECT, payload)],
        ..Recipe::default()
    }
}

// ---- Without a device -------------------------------------------------------------------------

/// Every spatial program the core ships, in every shape its units describe it (each unit alone and
/// together, the atmospheric light stored and taken on the GPU), passes the surface's own spatial
/// convention through the conversion.
#[test]
fn gpu_presence_the_program_passes_the_surfaces_own_convention() {
    let spatial: Vec<_> = GPU_PROGRAMS
        .iter()
        .filter(|program| program.kind == GpuProgramKind::Spatial)
        .collect();
    assert_eq!(
        spatial
            .iter()
            .map(|program| program.entry)
            .collect::<Vec<_>>(),
        ["lf_detail", "lf_presence"],
        "Detail's (`gpu_detail`) and Presence's are the spatial programs"
    );
    let registry = ModuleRegistry::builtin();
    for payload in [
        json!({"texture": 40}),
        json!({"clarity": -30}),
        json!({"dehaze": 25}),
        json!({"texture": -100, "clarity": 100, "dehaze": -100}),
    ] {
        for size in [stage(64, 48), stage(6000, 4000)] {
            let plan = match gpu_plan(
                &registry,
                &recipe(payload.clone()),
                GpuPlanRequest::exact(0, size).qualifying(),
            )
            .expect("the stack compiles")
            {
                GpuAnswer::Plan(plan) => *plan,
                GpuAnswer::Fallback(reason) => panic!("{payload}: {reason}"),
            };
            let held = boundary(64, 48, 1, &vec![[0.25; 3]; 64 * 48]).unwrap();
            let held = if size.width == 64 {
                held
            } else {
                // The conversion only checks the boundary's size; a 1-texel stand-in of the
                // plan's stage would do as well, but the convention is what is checked here.
                GpuBoundary::new(Arc::new(vec![0u8; 8 * 6000 * 4000]), 6000, 4000, 1).unwrap()
            };
            let converted = surface_plan(&plan, held).expect("a runnable plan");
            assert_eq!(converted.steps.len(), 1, "{payload}");
            validate_step(&converted.steps[0])
                .unwrap_or_else(|error| panic!("{payload} at {size:?}: {error}"));
        }
    }
    // Presence is enabled: it met the spatial limits on the corpus at Fit.
    assert!(presence().enabled);
    // A masked operation is one spatial step carrying its mask's coverage, which the surface's
    // convention accepts with the coverage programs'.
    let masked = masked(recipe(json!({"texture": 40})), &radial());
    let plan = match gpu_plan(&registry, &masked, GpuPlanRequest::exact(0, stage(64, 48))).unwrap()
    {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("{reason}"),
    };
    let held = boundary(64, 48, 1, &vec![[0.25; 3]; 64 * 48]).unwrap();
    let converted = surface_plan(&plan, held).expect("a runnable plan");
    let GpuStep::Spatial(step) = &converted.steps[0] else {
        panic!("a spatial step");
    };
    assert_eq!(
        step.mask.as_ref().map(|mask| mask.components.len()),
        Some(1)
    );
    validate_step(&converted.steps[0]).expect("the masked step");
}

/// A radial component: an ellipse a little off centre, tilted, with a broad feather.
fn radial() -> luxforge_core::Component {
    luxforge_core::Component::new(
        "Radial 1",
        luxforge_core::ComponentMode::Add,
        "radial",
        json!({"x": 0.45, "y": 0.55, "radius_x": 0.3, "radius_y": 0.22, "angle": 18.0,
               "feather": 45.0}),
    )
}

/// `recipe` with its Presence layer masked by one mask of `component`.
fn masked(mut recipe: Recipe, component: &luxforge_core::Component) -> Recipe {
    let mut mask = luxforge_core::Mask::new("Mask 1");
    mask.components.push(component.clone());
    recipe.layers[0].mask = Some(mask.id.clone());
    recipe.masks.push(mask);
    recipe
}

// ---- Per filter -------------------------------------------------------------------------------

/// The kernels a per-filter test adds to the core's program: one that loads its unit's input, the
/// boundary's colour, into a plane, and applies that show a plane, upsample one, and soft-clip the
/// boundary's own channels, each named after the program.
const TEST_KERNELS: &str = "
fn lf_presence_test_load(at: vec2<i32>, words: u32, block: u32) {
    lf_store(at, vec4<f32>(lf_source(at), 0.0));
}

fn lf_presence_test_show(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    return lf_plane(planes, at).xyz;
}

fn lf_presence_test_upsample(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    return vec3<f32>(lf_presence_upsample(planes, at, lf_f32(words)), 0.0, 0.0);
}

fn lf_presence_test_soft_clip(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    return vec3<f32>(lf_presence_soft_clip(rgb.x, rgb.y, rgb.z), 0.0, 0.0);
}
";

fn test_program(words: Vec<u32>) -> GpuProgram {
    GpuProgram {
        words,
        ..GpuProgram::new(
            "lf_presence",
            format!("{}{TEST_KERNELS}", presence().source),
        )
    }
}

fn full(format: PlaneFormat) -> GpuPlane {
    GpuPlane {
        format,
        size: PlaneSize::Reduced(1),
    }
}

fn reduced(format: PlaneFormat, s: u32) -> GpuPlane {
    GpuPlane {
        format,
        size: PlaneSize::Reduced(s),
    }
}

fn pass(
    kernel: &'static str,
    inputs: &[u32],
    output: u32,
    words: u32,
    shape: PassShape,
) -> GpuPass {
    GpuPass {
        kernel: Cow::Borrowed(kernel),
        inputs: inputs.to_vec(),
        output,
        words,
        source: 0,
        shape,
    }
}

fn across(run: u32) -> PassShape {
    PassShape::Texels { span: [run, 1] }
}

fn down(run: u32) -> PassShape {
    PassShape::Texels { span: [1, run] }
}

const EACH: PassShape = PassShape::Texels { span: [1, 1] };

/// The kernels' forms and finishes, as `presence.wgsl` names them.
const FORM_PLANE: u32 = 0;
const FORM_SQUARE: u32 = 1;
const FORM_GUIDED: u32 = 3;
const FINISH_MEAN: u32 = 0;
const FINISH_SELF: u32 = 1;
const FINISH_GUIDED: u32 = 2;
const FINISH_SMOOTH_PLANE: u32 = 4;
const REDUCE_ENCODED: u32 = 0;
const REDUCE_DARK: u32 = 2;

/// A plan of one spatial step over a boundary of `values`, its program the core's text with the
/// test kernels, read back as `f32`.
fn run_step(
    qualifier: &Qualifier,
    (width, height): (u32, u32),
    values: &[[f32; 3]],
    words: Vec<u32>,
    planes: Vec<GpuPlane>,
    passes: Vec<GpuPass>,
    apply: GpuApply,
) -> Vec<[f32; 4]> {
    let spatial = GpuSpatial {
        program: test_program(words),
        planes,
        passes,
        applies: vec![apply],
        clamps: false,
        mask: None,
    };
    let plan = GpuPlan {
        boundary: boundary(width, height, 1, values).expect("a boundary"),
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::Spatial(Box::new(spatial))],
    };
    qualifier.evaluate(&plan).expect("a qualification readback")
}

fn show(plane: u32) -> GpuApply {
    GpuApply {
        function: Cow::Borrowed("lf_presence_test_show"),
        planes: vec![plane],
        words: 0,
    }
}

/// A synthetic plane of `width × height` values in `[low, high)`, every texel one channel of the
/// boundary, held as the boundary holds it: smooth structure, a step edge and noise, so windows
/// see flat, ramped and busy neighbourhoods.
fn synthetic(width: u32, height: u32, seed: u64, low: f32, high: f32) -> Vec<f32> {
    let mut stream = Stream(seed);
    (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let ramp = (x as f32 / width as f32 + y as f32 / height as f32) * 0.5;
            let edge = if x > width / 3 && y < height * 2 / 3 {
                0.35
            } else {
                0.0
            };
            let noise = (stream.unit() as f32 - 0.5) * 0.3;
            let flat = x > width * 3 / 4 && y > height * 3 / 4;
            let unit = if flat {
                0.62
            } else {
                (ramp + edge + noise).clamp(0.0, 1.0)
            };
            held(low + (high - low) * unit)
        })
        .collect()
}

/// The largest absolute difference over the texels both hold, and where.
fn largest(gpu: &[f32], cpu: &[f32]) -> (f64, usize) {
    gpu.iter()
        .zip(cpu)
        .enumerate()
        .map(|(index, (g, c))| (f64::from((g - c).abs()), index))
        .fold((0.0, 0), |a, b| if b.0 > a.0 { b } else { a })
}

/// The box mean's two passes against the CPU's, at every radius the Presence units use from a
/// small Fit stage to a 60 MP one, each at several run lengths of its running sums.
#[test]
fn gpu_presence_box_mean_matches_the_cpu_filter() {
    let test = "gpu_presence_box_mean_matches_the_cpu_filter";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let (width, height) = (256, 176);
    let plane = synthetic(width, height, 0x51, -0.25, 1.5);
    let values: Vec<[f32; 3]> = plane.iter().map(|value| [*value, 0.0, 0.0]).collect();
    let mut worst_case = 0.0_f64;
    for r in [1, 2, 6, 14, 24, 38] {
        let cpu = cpu::box_mean(width, height, &plane, r);
        for run in [1, 4, 16, 64] {
            let words = vec![FORM_PLANE, r, run, FINISH_MEAN, r, run, 0];
            let gpu = run_step(
                &qualifier,
                (width, height),
                &values,
                words,
                vec![
                    full(PlaneFormat::Quad),
                    full(PlaneFormat::Quad),
                    full(PlaneFormat::Quad),
                ],
                vec![
                    pass("lf_presence_test_load", &[], 0, 0, EACH),
                    pass("lf_presence_sum_x", &[0], 1, 0, across(run)),
                    pass("lf_presence_sum_y", &[1], 2, 3, down(run)),
                ],
                show(2),
            );
            let gpu: Vec<f32> = gpu.iter().map(|texel| texel[0]).collect();
            let (difference, at) = largest(&gpu, &cpu);
            eprintln!(
                "box mean r {r:2} run {run:2}: largest {difference:.3e} at ({}, {})",
                at as u32 % width,
                at as u32 / width
            );
            worst_case = worst_case.max(difference);
        }
    }
    eprintln!("{test}: worst {worst_case:.3e}");
    assert!(
        worst_case < 2.0e-5,
        "the box mean differs by {worst_case:.3e}"
    );
}

/// The box minimum's horizontal pass and the transmission kernel's vertical one, which stores
/// `1 - omega * min` beside the encoded luminance of its colour. At `omega = 1` the subtraction is
/// one rounding however the backend fuses it, so the minimum itself is compared exactly; at any
/// other `omega` a fused multiply-add may round once where the CPU rounds twice.
#[test]
fn gpu_presence_box_minimum_matches_the_cpu_filter() {
    let test = "gpu_presence_box_minimum_matches_the_cpu_filter";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let (width, height) = (192, 128);
    let red = synthetic(width, height, 0x61, 0.0, 1.0);
    let green = synthetic(width, height, 0x62, 0.0, 1.0);
    let blue = synthetic(width, height, 0x63, 0.0, 1.0);
    let values: Vec<[f32; 3]> = (0..red.len())
        .map(|i| [red[i], green[i], blue[i]])
        .collect();
    let (mut exact, mut fused, mut guide) = (0.0_f64, 0.0_f64, 0.0_f64);
    for r in [1, 2, 3, 5] {
        let cpu = cpu::box_min(width, height, &red, r);
        for omega in [1.0_f32, 0.8] {
            let gpu = run_step(
                &qualifier,
                (width, height),
                &values,
                vec![0, r, r, omega.to_bits()],
                vec![
                    full(PlaneFormat::Quad),
                    full(PlaneFormat::Scalar),
                    full(PlaneFormat::Pair),
                ],
                vec![
                    pass("lf_presence_test_load", &[], 0, 0, EACH),
                    pass("lf_presence_min_x", &[0], 1, 0, EACH),
                    pass("lf_presence_dehaze_transmission", &[1, 0], 2, 2, EACH),
                ],
                show(2),
            );
            let raw: Vec<f32> = cpu.iter().map(|dark| 1.0 - omega * dark).collect();
            let transmission: Vec<f32> = gpu.iter().map(|texel| texel[1]).collect();
            let (difference, _) = largest(&transmission, &raw);
            if omega == 1.0 {
                exact = exact.max(difference);
            } else {
                fused = fused.max(difference);
            }
            let encoded: Vec<f32> = values
                .iter()
                .map(|rgb| cpu::encoded_luminance(*rgb))
                .collect();
            let shown: Vec<f32> = gpu.iter().map(|texel| texel[0]).collect();
            guide = guide.max(largest(&shown, &encoded).0);
        }
    }
    eprintln!("{test}: minimum {exact:.3e}, at omega 0.8 {fused:.3e}, guide {guide:.3e}");
    assert_eq!(exact, 0.0, "a minimum is exact");
    assert!(
        fused <= 1.2e-7,
        "one rounding of 1 - omega * min: {fused:.3e}"
    );
    assert!(
        guide < 1.0e-6,
        "the encoded luminance differs by {guide:.3e}"
    );
}

/// The self-guided smoother, the four passes Texture and Clarity run, against the CPU's, at
/// Texture's and Clarity's regularizations and the radii they reach.
#[test]
fn gpu_presence_self_guided_filter_matches_the_cpu_filter() {
    let test = "gpu_presence_self_guided_filter_matches_the_cpu_filter";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let (width, height) = (224, 160);
    let plane = synthetic(width, height, 0x71, 0.0, 1.0);
    let values: Vec<[f32; 3]> = plane.iter().map(|value| [*value, 0.0, 0.0]).collect();
    let mut worst_case = 0.0_f64;
    for (eps, radii) in [
        (cpu::EPS_TEXTURE, [1, 2, 6]),
        (cpu::EPS_CLARITY, [6, 24, 38]),
    ] {
        for r in radii {
            let cpu = cpu::guided_self(width, height, &plane, r, eps);
            for run in [1, 16] {
                let words = vec![
                    FORM_SQUARE,
                    r,
                    run,
                    FINISH_SELF,
                    r,
                    run,
                    eps.to_bits(),
                    FORM_PLANE,
                    r,
                    run,
                    FINISH_SMOOTH_PLANE,
                    r,
                    run,
                    0,
                ];
                let gpu = run_step(
                    &qualifier,
                    (width, height),
                    &values,
                    words,
                    vec![
                        full(PlaneFormat::Quad),
                        full(PlaneFormat::Pair),
                        full(PlaneFormat::Pair),
                        full(PlaneFormat::Scalar),
                    ],
                    vec![
                        pass("lf_presence_test_load", &[], 0, 0, EACH),
                        pass("lf_presence_sum_x", &[0], 1, 0, across(run)),
                        pass("lf_presence_sum_y", &[1], 2, 3, down(run)),
                        pass("lf_presence_sum_x", &[2], 1, 7, across(run)),
                        pass("lf_presence_sum_y", &[1, 0], 3, 10, down(run)),
                    ],
                    show(3),
                );
                let gpu: Vec<f32> = gpu.iter().map(|texel| texel[0]).collect();
                let (difference, _) = largest(&gpu, &cpu);
                eprintln!(
                    "self-guided eps {eps:.0e} r {r:2} run {run:2}: largest {difference:.3e}"
                );
                worst_case = worst_case.max(difference);
            }
        }
    }
    eprintln!("{test}: worst {worst_case:.3e}");
    assert!(
        worst_case < 5.0e-5,
        "the self-guided filter differs by {worst_case:.3e}"
    );
}

/// The guided filter Dehaze refines its transmission with, against the CPU's, at its
/// regularization.
#[test]
fn gpu_presence_guided_filter_matches_the_cpu_filter() {
    let test = "gpu_presence_guided_filter_matches_the_cpu_filter";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let (width, height) = (192, 144);
    let guide = synthetic(width, height, 0x81, 0.0, 1.0);
    let input = synthetic(width, height, 0x82, 0.1, 1.0);
    let values: Vec<[f32; 3]> = (0..guide.len())
        .map(|i| [guide[i], input[i], 0.0])
        .collect();
    let eps = cpu::EPS_DEHAZE;
    let mut worst_case = 0.0_f64;
    for r in [1, 3, 6, 10] {
        let cpu = cpu::guided_filter(width, height, &guide, &input, r, eps);
        for run in [1, 16] {
            let words = vec![
                FORM_GUIDED,
                r,
                run,
                FINISH_GUIDED,
                r,
                run,
                eps.to_bits(),
                FORM_PLANE,
                r,
                run,
                FINISH_SMOOTH_PLANE,
                r,
                run,
                0,
            ];
            let gpu = run_step(
                &qualifier,
                (width, height),
                &values,
                words,
                vec![
                    full(PlaneFormat::Quad),
                    full(PlaneFormat::Quad),
                    full(PlaneFormat::Pair),
                    full(PlaneFormat::Pair),
                    full(PlaneFormat::Scalar),
                ],
                vec![
                    pass("lf_presence_test_load", &[], 0, 0, EACH),
                    pass("lf_presence_sum_x", &[0], 1, 0, across(run)),
                    pass("lf_presence_sum_y", &[1], 2, 3, down(run)),
                    pass("lf_presence_sum_x", &[2], 3, 7, across(run)),
                    pass("lf_presence_sum_y", &[3, 0], 4, 10, down(run)),
                ],
                show(4),
            );
            let gpu: Vec<f32> = gpu.iter().map(|texel| texel[0]).collect();
            let (difference, _) = largest(&gpu, &cpu);
            eprintln!("guided r {r:2} run {run:2}: largest {difference:.3e}");
            worst_case = worst_case.max(difference);
        }
    }
    eprintln!("{test}: worst {worst_case:.3e}");
    assert!(
        worst_case < 5.0e-4,
        "the guided filter differs by {worst_case:.3e}"
    );
}

/// The 4x reduction of the encoded luminance Clarity smooths, and the 16x reduction of the colour
/// beside its channel minimum the atmospheric light is chosen from, against the CPU's block means.
#[test]
fn gpu_presence_block_reductions_match_the_cpu() {
    let test = "gpu_presence_block_reductions_match_the_cpu";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    // Neither side a multiple of 4 or 16, so the last blocks are partial.
    let (width, height) = (203, 131);
    let channels: Vec<Vec<f32>> = (0..3)
        .map(|c| synthetic(width, height, 0x91 + c, 0.0, 1.2))
        .collect();
    let values: Vec<[f32; 3]> = (0..channels[0].len())
        .map(|i| [channels[0][i], channels[1][i], channels[2][i]])
        .collect();
    // 4x of the encoded luminance.
    let encoded: Vec<f32> = values
        .iter()
        .map(|rgb| cpu::encoded_luminance(*rgb))
        .collect();
    let (rw, rh, cpu4) = cpu::downsample(width, height, &encoded, 4);
    let gpu = run_step(
        &qualifier,
        (width, height),
        &values,
        vec![REDUCE_ENCODED, 4],
        vec![reduced(PlaneFormat::Scalar, 4)],
        vec![pass("lf_presence_reduce", &[], 0, 0, EACH)],
        show(0),
    );
    let mut four = 0.0_f64;
    for j in 0..rh {
        for i in 0..rw {
            let g = gpu[(j * width + i) as usize][0];
            four = four.max(f64::from((g - cpu4[(j * rw + i) as usize]).abs()));
        }
    }
    // 16x of the colour and its minimum.
    let reduced16: Vec<(u32, u32, Vec<f32>)> = channels
        .iter()
        .map(|channel| cpu::downsample(width, height, channel, 16))
        .collect();
    let (sw, sh) = (reduced16[0].0, reduced16[0].1);
    let gpu = run_step(
        &qualifier,
        (width, height),
        &values,
        vec![REDUCE_DARK, 16],
        vec![reduced(PlaneFormat::Quad, 16)],
        vec![pass("lf_presence_reduce", &[], 0, 0, EACH)],
        show(0),
    );
    let mut sixteen = 0.0_f64;
    for j in 0..sh {
        for i in 0..sw {
            let g = gpu[(j * width + i) as usize];
            for (channel, reduced) in reduced16.iter().enumerate() {
                let c = reduced.2[(j * sw + i) as usize];
                sixteen = sixteen.max(f64::from((g[channel] - c).abs()));
            }
        }
    }
    eprintln!("{test}: 4x {four:.3e}, 16x {sixteen:.3e}");
    assert!(four < 1.0e-6 && sixteen < 1.0e-6);
}

/// The bilinear upsample of a reduced plane to the stage, against the CPU's.
#[test]
fn gpu_presence_upsample_matches_the_cpu() {
    let test = "gpu_presence_upsample_matches_the_cpu";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let (width, height) = (203_u32, 131_u32);
    let (rw, rh) = (width.div_ceil(4), height.div_ceil(4));
    let plane = synthetic(width, height, 0xa1, -0.2, 1.3);
    let values: Vec<[f32; 3]> = plane.iter().map(|value| [*value, 0.0, 0.0]).collect();
    // The reduced plane is the boundary's own top-left texels, which the loader writes.
    let source: Vec<f32> = (0..rh)
        .flat_map(|j| (0..rw).map(move |i| (i, j)))
        .map(|(i, j)| plane[(j * width + i) as usize])
        .collect();
    let cpu = cpu::upsample(width, height, rw, rh, &source, 4);
    let gpu = run_step(
        &qualifier,
        (width, height),
        &values,
        vec![4.0_f32.to_bits()],
        vec![reduced(PlaneFormat::Scalar, 4)],
        vec![pass("lf_presence_test_load", &[], 0, 0, EACH)],
        GpuApply {
            function: Cow::Borrowed("lf_presence_test_upsample"),
            planes: vec![0],
            words: 0,
        },
    );
    let gpu: Vec<f32> = gpu.iter().map(|texel| texel[0]).collect();
    let (difference, _) = largest(&gpu, &cpu);
    eprintln!("{test}: largest {difference:.3e}");
    assert!(difference < 1.0e-6);
}

/// The soft clip over a dense grid of excursions, encoded values (in range and out) and both
/// units' limits, against the CPU's, under this backend's `tanh`.
#[test]
fn gpu_presence_soft_clip_matches_the_cpu() {
    let test = "gpu_presence_soft_clip_matches_the_cpu";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let (width, height) = (256, 128);
    let values: Vec<[f32; 3]> = (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let raw = (x as f32 / (width - 1) as f32 - 0.5) * 8.0;
            let raw = if x == width / 2 { 0.0 } else { raw };
            let encoded = y as f32 / (height / 2 - 1) as f32 * 1.4 - 0.2;
            let limit = if y < height / 2 {
                cpu::LIMIT_TEXTURE
            } else {
                cpu::LIMIT_CLARITY
            };
            [held(raw), held(encoded.min(1.2)), held(limit)]
        })
        .collect();
    let gpu = run_step(
        &qualifier,
        (width, height),
        &values,
        vec![0],
        vec![full(PlaneFormat::Scalar)],
        vec![pass("lf_presence_test_load", &[], 0, 0, EACH)],
        GpuApply {
            function: Cow::Borrowed("lf_presence_test_soft_clip"),
            planes: vec![0],
            words: 0,
        },
    );
    let cpu: Vec<f32> = values
        .iter()
        .map(|[raw, encoded, limit]| cpu::soft_clip(*raw, *encoded, *limit))
        .collect();
    let gpu: Vec<f32> = gpu.iter().map(|texel| texel[0]).collect();
    let (difference, _) = largest(&gpu, &cpu);
    let zeros = gpu
        .iter()
        .zip(&cpu)
        .filter(|(g, c)| (**c == 0.0) != (**g == 0.0))
        .count();
    eprintln!("{test}: largest {difference:.3e}, zeros that differ {zeros}");
    assert!(difference < 2.0e-6 && zeros == 0);
}

/// The atmospheric light the GPU takes from the stage it holds: the 16x reduction, then one
/// workgroup's selection of the brightest dark-channel blocks, ties at the threshold taken in
/// row-major order. Against the CPU's preparation from the host's reduction of the same stage.
#[test]
fn gpu_presence_atmospheric_light_matches_the_cpu() {
    let test = "gpu_presence_atmospheric_light_matches_the_cpu";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let mut worst_case = 0.0_f64;
    for (case, (width, height), plateau) in [
        ("a small stage, the minimum count", (640, 480), false),
        ("a stage past 16 000 blocks", (2600, 1700), false),
        ("a bright plateau of tied blocks", (1300, 900), true),
    ] {
        let channels: Vec<Vec<f32>> = (0..3)
            .map(|c| {
                let mut plane = synthetic(width, height, 0xb1 + c, 0.0, 1.1);
                if plateau {
                    for y in 0..height / 4 {
                        for x in 0..width {
                            plane[(y * width + x) as usize] = held(0.97);
                        }
                    }
                }
                plane
            })
            .collect();
        let values: Vec<[f32; 3]> = (0..channels[0].len())
            .map(|i| [channels[0][i], channels[1][i], channels[2][i]])
            .collect();
        let planar: Vec<f32> = channels.concat();
        let cpu = cpu::atmosphere(width, height, &planar);
        let gpu = run_step(
            &qualifier,
            (width, height),
            &values,
            vec![REDUCE_DARK, 16, 1000, 16, 1.0e-3_f32.to_bits()],
            vec![
                reduced(PlaneFormat::Quad, 16),
                GpuPlane {
                    format: PlaneFormat::Quad,
                    size: PlaneSize::Fixed {
                        width: 1,
                        height: 1,
                    },
                },
            ],
            vec![
                pass("lf_presence_reduce", &[], 0, 0, EACH),
                pass("lf_presence_atmosphere", &[0], 1, 2, PassShape::Workgroup),
            ],
            GpuApply {
                function: Cow::Borrowed("lf_presence_test_show"),
                planes: vec![1],
                words: 0,
            },
        );
        let light = gpu[0];
        let difference = (0..3)
            .map(|channel| (f64::from(light[channel]) - cpu[channel]).abs())
            .fold(0.0, f64::max);
        eprintln!(
            "{case}: GPU {:?} CPU {cpu:?}: largest {difference:.3e}",
            &light[..3]
        );
        worst_case = worst_case.max(difference);
    }
    assert!(
        worst_case < 2.0e-5,
        "the atmospheric light differs by {worst_case:.3e}"
    );
}

// ---- Per unit ---------------------------------------------------------------------------------

/// A synthetic photograph for the units: a hazy sky over a darker ground, a hard horizon, fine
/// texture of a few pixels, a soft gradient and noise, in linear light, as RGB planes.
fn photograph(width: u32, height: u32, seed: u64) -> Vec<[f32; 3]> {
    let mut stream = Stream(seed);
    (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let sky = v < 0.4;
            let base: [f32; 3] = if sky {
                [0.55 + 0.2 * u, 0.62 + 0.15 * u, 0.75]
            } else {
                [0.12 + 0.1 * v, 0.16 + 0.05 * u, 0.08]
            };
            let texture = 0.04 * ((x as f32 * 1.7).sin() * (y as f32 * 2.3).cos());
            let noise = (stream.unit() as f32 - 0.5) * 0.03;
            let haze = 0.25 * (1.0 - v);
            base.map(|channel| {
                held((channel * (1.0 - haze) + 0.7 * haze + texture + noise).max(0.0))
            })
        })
        .collect()
}

/// What one unit case measured.
struct Measured {
    program: Statistics,
    drawn: Statistics,
    passed: bool,
    non_finite: usize,
    approximate: bool,
    planes: u64,
}

/// `stack` over `pixels` on the linear path: the CPU frame against the GPU plan's, its atmospheric
/// light stored by the CPU frame's render when `stored`, else taken on the GPU.
fn measure_unit(
    qualifier: &Qualifier,
    registry: &ModuleRegistry,
    stack: &Recipe,
    (width, height): (u32, u32),
    pixels: &[[f32; 3]],
    stored: bool,
) -> Measured {
    let len = pixels.len();
    let mut planes = vec![0.0_f32; 3 * len];
    for (index, pixel) in pixels.iter().enumerate() {
        for channel in 0..3 {
            planes[channel * len + index] = pixel[channel];
        }
    }
    let image = LinearImage::with_fingerprint(width, height, planes, "sha256:gpu-presence")
        .expect("a linear image");
    let source = RenderSource::Linear {
        image: &image,
        settings: LinearSettings::default(),
    };
    let context = RenderContext::new();
    let cpu = render(
        registry,
        source,
        stack,
        RenderOptions::exact(&Cancel::never()),
        &context,
    )
    .and_then(|render| render.frame(SnapshotId::new()))
    .expect("the CPU frame");
    let fresh = RenderContext::new();
    let estimates = GpuEstimates {
        context: if stored { &context } else { &fresh },
        source,
    };
    let plan = match gpu_plan_with(
        registry,
        stack,
        GpuPlanRequest::exact(0, stage(width, height))
            .qualifying()
            .linear(),
        Some(estimates),
    )
    .expect("the stack compiles")
    {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("{reason}"),
    };
    let approximate = plan.approximate();
    let planes = plan
        .spatial
        .as_ref()
        .map_or(0, |spatial| spatial.plane_bytes((0, 0), (width, height)));
    let held_boundary = boundary(width, height, 1, pixels).expect("a boundary");
    let converted = surface_plan(&plan, held_boundary).expect("a runnable plan");
    let gpu = qualifier.evaluate(&converted).expect("a readback");
    let drawn = qualifier.evaluate_codes(&converted).expect("a readback");
    let non_finite = gpu
        .iter()
        .flat_map(|texel| texel[..3].iter())
        .filter(|value| !value.is_finite())
        .count();
    let reference: Vec<u8> = cpu
        .rgba
        .chunks_exact(4)
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
        .collect();
    let program = codes(gpu.iter().map(|texel| [texel[0], texel[1], texel[2]]));
    let candidate: Vec<u8> = drawn
        .iter()
        .flat_map(|code| [code[0], code[1], code[2]])
        .collect();
    let frame = |bytes| Rgb8::new(width, height, bytes).expect("a whole frame");
    let compare = |candidate| {
        preview_error::compare(frame(candidate), frame(&reference), [0, 0, width, height])
            .expect("comparable frames")
    };
    let program = compare(&program);
    Measured {
        passed: preview_error::verdict(&program, Class::Spatial).passed() && non_finite == 0,
        drawn: compare(&candidate),
        program,
        non_finite,
        approximate,
        planes,
    }
}

/// Every Presence combination at both ends, unmasked and masked by a feathered radial, over a
/// synthetic photograph at two sizes, against the CPU frame of the same stack: the program's `f32`
/// output through the reference quantizer held to the spatial limits and the finiteness rule, the
/// drawn codes reported beside it. Dehaze is run with the light the CPU stored and with the one
/// the GPU takes from the stage it holds, which on a whole stage is the same selection.
#[test]
fn gpu_presence_units_meet_the_spatial_limits() {
    let test = "gpu_presence_units_meet_the_spatial_limits";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let registry = ModuleRegistry::builtin();
    let mut cases = Vec::new();
    for amount in [100, -100] {
        cases.push(json!({"texture": amount}));
        cases.push(json!({"clarity": amount}));
        cases.push(json!({"dehaze": amount}));
        cases.push(json!({"texture": amount, "clarity": amount}));
        cases.push(json!({"texture": amount, "clarity": amount, "dehaze": amount}));
    }
    cases.push(json!({"texture": 35, "clarity": -60, "dehaze": 20}));
    let (mut programs, mut drawn, mut missed) = (Vec::new(), Vec::new(), Vec::new());
    for (width, height) in [(480, 320), (1536, 1024)] {
        let pixels = photograph(width, height, u64::from(width));
        for (payload, masking) in cases
            .iter()
            .flat_map(|payload| [(payload, false), (payload, true)])
        {
            let dehaze = payload.get("dehaze").is_some();
            let stack = if masking {
                masked(recipe(payload.clone()), &radial())
            } else {
                recipe(payload.clone())
            };
            for stored in if dehaze {
                vec![true, false]
            } else {
                vec![true]
            } {
                let measured = measure_unit(
                    &qualifier,
                    &registry,
                    &stack,
                    (width, height),
                    &pixels,
                    stored,
                );
                let name = format!(
                    "{payload}{} at {width}x{height}{}",
                    if masking { " masked" } else { "" },
                    match (dehaze, stored) {
                        (true, true) => ", light stored",
                        (true, false) => ", light taken on the GPU",
                        _ => "",
                    }
                );
                assert_eq!(measured.approximate, dehaze && !stored, "{name}");
                eprintln!(
                    "{name}: program {} | drawn {} | non-finite {} | planes {} B{}",
                    figures(&measured.program),
                    figures(&measured.drawn),
                    measured.non_finite,
                    measured.planes,
                    if measured.passed { "" } else { " MISS" }
                );
                programs.push(measured.program);
                drawn.push(measured.drawn);
                if !measured.passed {
                    missed.push(name);
                }
            }
        }
    }
    eprintln!(
        "{test}: worst of {} cases: program {} | drawn {}",
        programs.len(),
        figures(&worst(&programs)),
        figures(&worst(&drawn))
    );
    assert!(
        missed.is_empty(),
        "cases missing the spatial limits: {missed:?}"
    );
}

/// The same stacks on the byte path, over an 8-bit source: the operation's input and output are
/// clamped where the CPU quantizes its frames.
#[test]
fn gpu_presence_on_the_byte_path_meets_the_spatial_limits() {
    let test = "gpu_presence_on_the_byte_path_meets_the_spatial_limits";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (960, 640);
    let pixels = photograph(width, height, 7);
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
        fingerprint: "sha256:gpu-presence-bytes".into(),
        orientation: 1,
        capture: Arc::default(),
    };
    let table = luxforge_core::colour::srgb::decode_table();
    let texels: Vec<[f32; 3]> = rgba
        .chunks_exact(4)
        .map(|pixel| [0, 1, 2].map(|c| table[usize::from(pixel[c])]))
        .collect();
    let mut statistics = Vec::new();
    let mut missed = Vec::new();
    for payload in [
        json!({"texture": 100, "clarity": 100, "dehaze": 100}),
        json!({"texture": -100, "clarity": -100, "dehaze": -100}),
    ] {
        let stack = recipe(payload.clone());
        let context = RenderContext::new();
        let cpu = render(
            &registry,
            &image,
            &stack,
            RenderOptions::exact(&Cancel::never()),
            &context,
        )
        .and_then(|render| render.frame(SnapshotId::new()))
        .expect("the CPU frame");
        let plan = match gpu_plan_with(
            &registry,
            &stack,
            GpuPlanRequest::exact(0, stage(width, height)).qualifying(),
            Some(GpuEstimates {
                context: &context,
                source: RenderSource::Byte(&image),
            }),
        )
        .unwrap()
        {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
        assert!(plan.spatial.as_ref().unwrap().clamps && !plan.approximate());
        let converted = surface_plan(&plan, boundary(width, height, 1, &texels).unwrap()).unwrap();
        let gpu = qualifier.evaluate(&converted).unwrap();
        let reference: Vec<u8> = cpu
            .rgba
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        let program = codes(gpu.iter().map(|texel| [texel[0], texel[1], texel[2]]));
        let frame = |bytes| Rgb8::new(width, height, bytes).unwrap();
        let measured =
            preview_error::compare(frame(&program), frame(&reference), [0, 0, width, height])
                .unwrap();
        eprintln!("{payload}: program {}", figures(&measured));
        if !preview_error::verdict(&measured, Class::Spatial).passed() {
            missed.push(payload.to_string());
        }
        statistics.push(measured);
    }
    eprintln!("{test}: worst {}", figures(&worst(&statistics)));
    assert!(missed.is_empty(), "{missed:?}");
}

/// Presence's pass pipelines are their kernels and shapes: all three units compile fewer pipelines
/// than they run passes, a drag that changes only amounts compiles none, and the plan whose light
/// is taken on the GPU adds only the light's own two passes to the plan whose light was stored.
#[test]
fn gpu_presence_pass_pipelines_are_shared_across_plans() {
    let test = "gpu_presence_pass_pipelines_are_shared_across_plans";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (480, 320);
    let pixels = photograph(width, height, 3);
    let len = pixels.len();
    let mut planes = vec![0.0_f32; 3 * len];
    for (index, pixel) in pixels.iter().enumerate() {
        for channel in 0..3 {
            planes[channel * len + index] = pixel[channel];
        }
    }
    let image = LinearImage::with_fingerprint(width, height, planes, "sha256:gpu-presence-passes")
        .expect("a linear image");
    let source = RenderSource::Linear {
        image: &image,
        settings: LinearSettings::default(),
    };
    let stack = recipe(json!({"texture": 40, "clarity": 30, "dehaze": 25}));
    let stored = RenderContext::new();
    render(
        &registry,
        source,
        &stack,
        RenderOptions::exact(&Cancel::never()),
        &stored,
    )
    .and_then(|render| render.frame(SnapshotId::new()))
    .expect("the CPU frame, which stores the light");
    let fresh = RenderContext::new();
    let converted = |stack: &Recipe, context: &RenderContext| {
        let plan = match gpu_plan_with(
            &registry,
            stack,
            GpuPlanRequest::exact(0, stage(width, height))
                .qualifying()
                .linear(),
            Some(GpuEstimates { context, source }),
        )
        .expect("the stack compiles")
        {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
        let passes = plan
            .spatial
            .as_ref()
            .map_or(0, |spatial| spatial.passes.len());
        let held = boundary(width, height, 1, &pixels).expect("a boundary");
        (surface_plan(&plan, held).expect("a runnable plan"), passes)
    };
    let (plan, passes) = converted(&stack, &stored);
    qualifier.evaluate(&plan).expect("a readback");
    let first = qualifier.pass_pipelines_created();
    eprintln!("{test}: all three, light stored: {passes} passes, {first} pipelines");
    assert!(first < passes as u64);
    // A drag moves amounts: its words change, its modules do not.
    let mut dragged = stack.clone();
    dragged.layers[0].payload = json!({"texture": 75, "clarity": -10, "dehaze": 60});
    qualifier
        .evaluate(&converted(&dragged, &stored).0)
        .expect("a readback");
    assert_eq!(
        qualifier.pass_pipelines_created(),
        first,
        "a drag compiles nothing"
    );
    // The light taken on the GPU: its reduction and its workgroup are new, every other pass is
    // one already compiled, though the words after Dehaze's sit at other offsets.
    let (plan, passes) = converted(&stack, &fresh);
    qualifier.evaluate(&plan).expect("a readback");
    let second = qualifier.pass_pipelines_created();
    eprintln!(
        "{test}: all three, light taken on the GPU: {passes} passes, {} more pipelines",
        second - first
    );
    assert_eq!(second - first, 2);
}

// ---- The corpus at Fit ------------------------------------------------------------------------

/// The qualification corpus's Presence recipes at Fit through the shared harness
/// ([`corpus_at_fit`]), each held to the spatial limits.
///
/// ```sh
/// LUXFORGE_GPU_CORPUS_OUTPUT=/tmp/new-dir \
/// LUXFORGE_GENERATED_FIXTURES=fixtures/generated \
/// LUXFORGE_RAW_MANIFEST=/path/to/raw-manifest.json \
/// cargo test -p luxforge-app gpu_presence_corpus -- --ignored --nocapture
/// ```
#[test]
#[ignore = "the GPU preview corpus at Fit: set LUXFORGE_GPU_CORPUS_OUTPUT to a new directory, \
            LUXFORGE_GENERATED_FIXTURES to the generated JPEGs and, for the RAWs, \
            LUXFORGE_RAW_MANIFEST"]
fn gpu_presence_corpus_at_fit() {
    corpus_at_fit("gpu_presence_corpus_at_fit", &["presence"]);
}
