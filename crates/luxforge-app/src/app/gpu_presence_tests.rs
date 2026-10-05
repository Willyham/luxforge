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
//!   the same stack, judged by the spatial limits, Dehaze's atmospheric light its light link's,
//!   computed from the whole stage.
//!
//! A test with no adapter prints that it was skipped and asserts nothing: the skip is the report,
//! and `cargo test` counting it as passed does not make it GPU evidence.
use super::gpu_plan::surface_plan;
use super::gpu_qualification::{
    Stream, codes, corpus_at_fit, differing, drafted_against_cpu, figures, lit, lit_fixed, worst,
};
use luxforge_core::{
    Cancel, GPU_PROGRAMS, GpuAnswer, GpuPlanRequest, GpuProgramKind, Layer, LinearImage,
    LinearSettings, ModuleRegistry, PRESENCE_EFFECT, PreviewSource, Recipe, RenderContext,
    RenderOptions, RenderSource, SnapshotId, SourceImage, Stage, gpu_plan,
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
/// together, Dehaze reading its light from the plane its light link writes), passes the surface's own spatial
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
                GpuBoundary::new(
                    Arc::new(vec![0u8; 8 * 6000 * 4000]),
                    6000,
                    4000,
                    1,
                    luxforge_ui::photo_surface::BoundaryFormat::Half,
                )
                .unwrap()
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

/// A pass of `kernel`: the loader and the reduction read the unit's input, and every other kernel
/// these tests run reads planes alone.
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
        reads_source: matches!(kernel, "lf_presence_test_load" | "lf_presence_reduce"),
        shape,
        unit: 0,
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
const REDUCE_BLOCKS: u32 = 4;

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
        halos: Vec::new(),
    };
    let plan = GpuPlan {
        boundary: boundary(width, height, 1, values).expect("a boundary"),
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::Spatial(Box::new(spatial))],
        region: None,
        lights: Vec::new(),
    };
    qualifier.evaluate(&plan).expect("a qualification readback")
}

fn show(plane: u32) -> GpuApply {
    GpuApply {
        function: Cow::Borrowed("lf_presence_test_show"),
        planes: vec![plane],
        words: 0,
        identity: false,
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
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
        vec![REDUCE_BLOCKS, 16, width, height],
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
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
            identity: false,
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
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
            identity: false,
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

/// The atmospheric light a light link computes from the whole stage: the 16x reduction into the
/// stage's block plane, then one workgroup's selection of the brightest dark-channel blocks, ties
/// at the threshold taken in row-major order. Against the CPU's preparation from the host's
/// reduction of the same stage.
#[test]
fn gpu_presence_atmospheric_light_matches_the_cpu() {
    let test = "gpu_presence_atmospheric_light_matches_the_cpu";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
        // The words of the reduction over the whole stage's grid and of the atmosphere's pass.
        let run = || {
            run_step(
                &qualifier,
                (width, height),
                &values,
                vec![
                    REDUCE_BLOCKS,
                    16,
                    width,
                    height,
                    1000,
                    16,
                    1.0e-3_f32.to_bits(),
                ],
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
                    pass("lf_presence_atmosphere", &[0], 1, 4, PassShape::Workgroup),
                ],
                GpuApply {
                    function: Cow::Borrowed("lf_presence_test_show"),
                    planes: vec![1],
                    words: 0,
                    identity: false,
                },
            )
        };
        let gpu = run();
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
    planes: u64,
}

/// `stack` over `pixels` on the linear path: the CPU frame against the GPU plan's, its atmospheric
/// light its light link's, computed from the whole stage by the surface's own link.
fn measure_unit(
    qualifier: &Qualifier,
    registry: &ModuleRegistry,
    stack: &Recipe,
    (width, height): (u32, u32),
    pixels: &[[f32; 3]],
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
    let plan = match gpu_plan(
        registry,
        stack,
        GpuPlanRequest::exact(0, stage(width, height))
            .qualifying()
            .linear(),
    )
    .expect("the stack compiles")
    {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("{reason}"),
    };
    let planes = plan
        .spatial
        .iter()
        .map(|spatial| spatial.plane_bytes((0, 0), (width, height)))
        .sum::<u64>();
    let held_boundary = boundary(width, height, 1, pixels).expect("a boundary");
    let converted = surface_plan(&plan, held_boundary).expect("a runnable plan");
    let preview = PreviewSource::Raw {
        image: image.clone(),
        settings: LinearSettings::default(),
    };
    lit(qualifier, &preview, &converted).expect("the plan's lights");
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
        planes,
    }
}

/// Every Presence combination at both ends, unmasked and masked by a feathered radial, over a
/// synthetic photograph at two sizes, against the CPU frame of the same stack: the program's `f32`
/// output through the reference quantizer held to the spatial limits and the finiteness rule, the
/// drawn codes reported beside it. Dehaze reads the light its light link computes from the whole
/// stage.
#[test]
fn gpu_presence_units_meet_the_spatial_limits() {
    let test = "gpu_presence_units_meet_the_spatial_limits";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
            let stack = if masking {
                masked(recipe(payload.clone()), &radial())
            } else {
                recipe(payload.clone())
            };
            let measured = measure_unit(&qualifier, &registry, &stack, (width, height), &pixels);
            let name = format!(
                "{payload}{} at {width}x{height}",
                if masking { " masked" } else { "" },
            );
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
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
        let plan = match gpu_plan(
            &registry,
            &stack,
            GpuPlanRequest::exact(0, stage(width, height)).qualifying(),
        )
        .unwrap()
        {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
        assert!(plan.spatial.first().unwrap().clamps && plan.reads_lights());
        let converted = surface_plan(&plan, boundary(width, height, 1, &texels).unwrap()).unwrap();
        lit(&qualifier, &PreviewSource::Jpeg(image.clone()), &converted).unwrap();
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
/// than they run passes, and a drag that changes only amounts compiles none. A step before Presence is a link of its own, whose output Presence's link reads as its
/// boundary, so a plan with Basic before it compiles none of Presence's passes again, and one with
/// Detail before it only Detail's own.
#[test]
fn gpu_presence_pass_pipelines_are_shared_across_plans() {
    let test = "gpu_presence_pass_pipelines_are_shared_across_plans";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
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
    let source = PreviewSource::Raw {
        image,
        settings: LinearSettings::default(),
    };
    let stack = recipe(json!({"texture": 40, "clarity": 30, "dehaze": 25}));
    // Each plan with its light, which a light link computes from the whole stage on a pipeline of
    // its own: only the plan's passes are this qualifier's.
    let converted = |stack: &Recipe| {
        let plan = match gpu_plan(
            &registry,
            stack,
            GpuPlanRequest::exact(0, stage(width, height))
                .qualifying()
                .linear(),
        )
        .expect("the stack compiles")
        {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
        let passes = plan
            .spatial
            .iter()
            .map(|spatial| spatial.passes.len())
            .sum::<usize>();
        let held = boundary(width, height, 1, &pixels).expect("a boundary");
        let converted = surface_plan(&plan, held).expect("a runnable plan");
        lit(&qualifier, &source, &converted).expect("the plan's light");
        (converted, passes)
    };
    let (plan, passes) = converted(&stack);
    qualifier.evaluate(&plan).expect("a readback");
    let first = qualifier.pass_pipelines_created();
    eprintln!("{test}: all three: {passes} passes, {first} pipelines");
    assert!(first < passes as u64);
    // A drag moves amounts: its words change, its modules do not.
    let mut dragged = stack.clone();
    dragged.layers[0].payload = json!({"texture": 75, "clarity": -10, "dehaze": 60});
    qualifier
        .evaluate(&converted(&dragged).0)
        .expect("a readback");
    let second = qualifier.pass_pipelines_created();
    assert_eq!(second, first, "a drag compiles nothing");
    // Basic before Presence: Presence's link reads what Basic's wrote, so every one of its passes
    // runs a pipeline above.
    let mut lifted = stack.clone();
    lifted.layers.insert(
        0,
        Layer::new(luxforge_core::BASIC_EFFECT, json!({"exposure": 0.3})),
    );
    let (plan, lifted_passes) = converted(&lifted);
    qualifier.evaluate(&plan).expect("a readback");
    let third = qualifier.pass_pipelines_created();
    eprintln!(
        "{test}: Basic, then all three: {lifted_passes} passes, {} more pipelines",
        third - second
    );
    assert_eq!((lifted_passes, third - second), (passes, 0));
    // Detail before Presence: Detail's six, Presence's link running every one of its own above.
    let mut detailed = stack.clone();
    detailed.layers.insert(
        0,
        Layer::new(
            luxforge_core::DETAIL_EFFECT,
            json!({"luminance": 40, "colour": 40, "sharpening": 50}),
        ),
    );
    let (plan, detailed_passes) = converted(&detailed);
    qualifier.evaluate(&plan).expect("a readback");
    let fourth = qualifier.pass_pipelines_created();
    eprintln!(
        "{test}: Detail, then all three: {detailed_passes} passes, {} more pipelines",
        fourth - third
    );
    assert_eq!(fourth - third, 6);
}

/// Every pass Presence's and Detail's units describe as reading only planes reads no input: a plan
/// of both after a colour layer draws exactly what it draws with every pass reading its unit's
/// input through everything before it. A pass that reads its input while described otherwise
/// reads the stub its module then declares, whose values no photograph holds, and its frame would
/// differ.
#[test]
fn gpu_presence_passes_that_read_only_planes_read_no_input() {
    let test = "gpu_presence_passes_that_read_only_planes_read_no_input";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (240, 160);
    let pixels = photograph(width, height, 11);
    let stack = Recipe {
        layers: vec![
            Layer::new(luxforge_core::BASIC_EFFECT, json!({"exposure": 0.3})),
            Layer::new(
                luxforge_core::DETAIL_EFFECT,
                json!({"luminance": 40, "colour": 40, "sharpening": 50}),
            ),
            Layer::new(
                PRESENCE_EFFECT,
                json!({"texture": 40, "clarity": 30, "dehaze": 25}),
            ),
        ],
        ..Recipe::default()
    };
    let request = GpuPlanRequest::exact(0, stage(width, height))
        .qualifying()
        .linear();
    let Ok(GpuAnswer::Plan(plan)) = gpu_plan(&registry, &stack, request) else {
        panic!("{test}: a plan");
    };
    let described = surface_plan(&plan, boundary(width, height, 1, &pixels).unwrap()).unwrap();
    let mut reading = described.clone();
    let mut planes_alone = 0;
    for step in &mut reading.steps {
        if let GpuStep::Spatial(spatial) = step {
            for pass in &mut spatial.passes {
                planes_alone += usize::from(!pass.reads_source);
                pass.reads_source = true;
            }
        }
    }
    // Both read one light, whichever it is.
    lit_fixed(&qualifier, &described);
    let drawn = qualifier.evaluate(&described).expect("a readback");
    let read = qualifier.evaluate(&reading).expect("a readback");
    let differing = drawn
        .iter()
        .zip(&read)
        .filter(|(drawn, read)| drawn != read)
        .count();
    eprintln!(
        "{test}: {planes_alone} passes read only planes; {differing} of {} texels differ",
        drawn.len()
    );
    assert!(planes_alone > 0);
    assert_eq!(differing, 0);
}

/// Texture's band is an `f32` in a plane of one channel or two: a plan drawn with the band's plane
/// edited back to the two-channel `rg32float` it once took draws the frame its one-channel
/// `r32float` plane draws, bit for bit, and its slot charges 4 bytes a texel more. Texture alone
/// and with Clarity and Dehaze, unmasked and masked by a feathered radial, on the byte path, the
/// linear path and in the GPU shape a drag draws.
#[test]
fn gpu_presence_texture_band_in_one_channel_draws_what_two_did() {
    let test = "gpu_presence_texture_band_in_one_channel_draws_what_two_did";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (480, 320);
    let pixels = photograph(width, height, 13);
    let held = boundary(width, height, 1, &pixels).expect("a boundary");
    let at = GpuPlanRequest::exact(0, stage(width, height));
    for payload in [
        json!({"texture": 60}),
        json!({"texture": -45, "clarity": 40, "dehaze": 30}),
    ] {
        for masking in [false, true] {
            let stack = if masking {
                masked(recipe(payload.clone()), &radial())
            } else {
                recipe(payload.clone())
            };
            for (path, request) in [
                ("byte", at),
                ("linear", at.linear()),
                ("drafted", at.linear().drafted(0)),
            ] {
                let plan = match gpu_plan(&registry, &stack, request).unwrap() {
                    GpuAnswer::Plan(plan) => *plan,
                    GpuAnswer::Fallback(reason) => panic!("{reason}"),
                };
                let one = surface_plan(&plan, held.clone()).expect("a runnable plan");
                let mut two = one.clone();
                let GpuStep::Spatial(spatial) = &mut two.steps[0] else {
                    panic!("a spatial step first");
                };
                let texture = spatial
                    .applies
                    .iter()
                    .find(|apply| apply.function == "lf_presence_texture")
                    .expect("Texture's apply");
                let [band] = texture.planes[..] else {
                    panic!("Texture's apply reads its band alone");
                };
                let band = &mut spatial.planes[band as usize];
                assert_eq!(band.format, PlaneFormat::HalfScalar);
                band.format = PlaneFormat::HalfPair;
                lit_fixed(&qualifier, &one);
                let (left, right) = (
                    qualifier.evaluate(&one).expect("a readback"),
                    qualifier.evaluate(&two).expect("a readback"),
                );
                let differing = left
                    .iter()
                    .zip(&right)
                    .filter(|(a, b)| a.map(f32::to_bits) != b.map(f32::to_bits))
                    .count();
                let charged = |plan: &GpuPlan| qualifier.charged_bytes(plan).expect("a charge");
                let more = charged(&two) - charged(&one);
                let name = format!(
                    "{payload}{} on the {path} path",
                    if masking { " masked" } else { "" }
                );
                eprintln!(
                    "{test}: {name}: {differing} of {} texels differ; two channels charge {more} B \
                     more",
                    left.len()
                );
                assert_eq!(differing, 0, "{name}");
                assert_eq!(more, 4 * u64::from(width) * u64::from(height), "{name}");
            }
        }
    }
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

/// A tick runs only the passes whose words or inputs changed: after the plan a drag started from,
/// a change to an amount reruns only the passes that read it, directly or through a unit's input,
/// and draws exactly what a slot that ran every pass draws. Of all three units' 20 passes, a
/// Clarity drag runs none, a Texture drag 5 and a Dehaze drag 19, the light read from its plane:
/// one link alone wrote its scratch last, so the pool it takes it from changes no count.
#[test]
fn gpu_presence_a_drag_reruns_only_the_passes_it_changes() {
    let test = "gpu_presence_a_drag_reruns_only_the_passes_it_changes";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
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
    let image = LinearImage::with_fingerprint(width, height, planes, "sha256:gpu-presence-drag")
        .expect("a linear image");
    let source = PreviewSource::Raw {
        image,
        settings: LinearSettings::default(),
    };
    let start = json!({"texture": 40, "clarity": 30, "dehaze": 25});
    let mut stack = recipe(start.clone());
    let mut converted = |payload: &Value| {
        // One recipe, its payload changed in place, as a drag changes it.
        stack.layers[0].payload = payload.clone();
        let plan = match gpu_plan(
            &registry,
            &stack,
            GpuPlanRequest::exact(0, stage(width, height))
                .qualifying()
                .linear(),
        )
        .expect("the stack compiles")
        {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
        let passes = plan
            .spatial
            .iter()
            .map(|spatial| spatial.passes.len() as u64)
            .sum::<u64>();
        let held = boundary(width, height, 1, &pixels).expect("a boundary");
        (surface_plan(&plan, held).expect("a runnable plan"), passes)
    };
    // Each drag: where it goes from the start, and how many passes it runs.
    let drags = [
        (
            "Clarity",
            json!({"texture": 40, "clarity": -10, "dehaze": 25}),
            0,
        ),
        (
            "Texture",
            json!({"texture": 75, "clarity": 30, "dehaze": 25}),
            5,
        ),
        (
            "Dehaze",
            json!({"texture": 40, "clarity": 30, "dehaze": 60}),
            19,
        ),
        (
            "all three",
            json!({"texture": 75, "clarity": -10, "dehaze": 60}),
            19,
        ),
        ("nothing", start.clone(), 0),
    ];
    for (drag, payload, wanted) in drags {
        let (first, all) = converted(&start);
        // The light depends on the stage alone, which no drag here moves: one light for both.
        lit(&qualifier, &source, &first).expect("the plan's light");
        let (then, _) = converted(&payload);
        let (after, ran) = qualifier
            .evaluate_after(&first, &then)
            .expect("a readback after the first");
        let whole = qualifier.evaluate(&then).expect("a readback of every pass");
        eprintln!("{test}: {drag}: {ran} of {all} passes");
        assert_eq!((ran, all), (wanted, 20), "{drag}");
        assert_eq!(
            differing(&after, &whole),
            0,
            "{drag}: texels that differ from every pass run"
        );
    }
}

/// A drafted layer's amount-0 units are the identity through applies that read no plane, so a tick
/// runs none of the passes only they need: a Dehaze drag with Texture and Clarity at zero runs only
/// the Dehaze passes its words change, as many as a drag of Dehaze alone in the CPU's shape, and a
/// Texture drag with Clarity at zero none. Within one drag, a unit leaving zero runs, in that tick,
/// every pass whose input moved while it was neutral, or every one when it has never run; one
/// returning to a value its planes still hold runs none. Every tick draws, bit for bit, what a run
/// of every pass draws, and so it does with the poison on, the scratch pool's textures holding NaN
/// before each tick's passes.
#[test]
fn gpu_presence_a_neutral_unit_runs_no_pass_until_it_leaves_zero() {
    let test = "gpu_presence_a_neutral_unit_runs_no_pass_until_it_leaves_zero";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
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
    let image = LinearImage::with_fingerprint(width, height, planes, "sha256:gpu-presence-neutral")
        .expect("a linear image");
    let source = PreviewSource::Raw {
        image,
        settings: LinearSettings::default(),
    };
    // One recipe, its payload changed in place, as a drag changes it; drafted, the layer holds
    // every unit, an amount-0 one the identity.
    let stack = std::cell::RefCell::new(recipe(json!({})));
    let plan = |payload: &Value, drafted: bool| {
        let mut stack = stack.borrow_mut();
        stack.layers[0].payload = payload.clone();
        let request = GpuPlanRequest::exact(0, stage(width, height))
            .qualifying()
            .linear();
        let request = if drafted { request.drafted(0) } else { request };
        let plan = match gpu_plan(&registry, &stack, request).expect("the stack compiles") {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{reason}"),
        };
        let held = boundary(width, height, 1, &pixels).expect("a boundary");
        surface_plan(&plan, held).expect("a runnable plan")
    };
    // The light depends on the stage alone, which no tick here moves: one light for every plan.
    lit(&qualifier, &source, &plan(&json!({"dehaze": 25}), true)).expect("the light");
    let passes = |plan: &GpuPlan| {
        plan.steps
            .iter()
            .map(|step| match step {
                GpuStep::Spatial(spatial) => spatial.passes.len() as u64,
                _ => 0,
            })
            .sum::<u64>()
    };
    // The plan with no apply the identity: every pass runs, and an amount-0 unit's apply still
    // returns its input through its words.
    let every = |plan: &GpuPlan| {
        let mut plan = plan.clone();
        for step in &mut plan.steps {
            if let GpuStep::Spatial(spatial) = step {
                for apply in &mut spatial.applies {
                    apply.identity = false;
                }
            }
        }
        plan
    };
    // Each unit's passes, as the CPU's shape of it alone holds them, and what a drag of Dehaze
    // alone reruns there: the passes its words change.
    let [dehaze, texture, clarity] = [
        json!({"dehaze": 25}),
        json!({"texture": 40}),
        json!({"clarity": 30}),
    ]
    .map(|payload| passes(&plan(&payload, false)));
    let all = dehaze + texture + clarity;
    let (_, moved) = qualifier
        .evaluate_after(
            &plan(&json!({"dehaze": 25}), false),
            &plan(&json!({"dehaze": 60}), false),
        )
        .expect("a readback of Dehaze alone");
    eprintln!(
        "{test}: Dehaze {dehaze}, Texture {texture} and Clarity {clarity} passes; a drag of \
         Dehaze alone reruns {moved}"
    );
    assert!(0 < moved && moved < dehaze, "{moved} of {dehaze}");
    // Each drag: its ticks, each with the passes it may run.
    let drags = [
        (
            "Dehaze, Texture and Clarity at zero",
            vec![
                (json!({"dehaze": 25}), dehaze),
                (json!({"dehaze": 60}), moved),
            ],
        ),
        (
            "Texture, Clarity at zero",
            vec![
                (json!({"texture": 40, "dehaze": 25}), dehaze + texture),
                (json!({"texture": 75, "dehaze": 25}), 0),
            ],
        ),
        (
            "across zero",
            vec![
                (json!({"texture": 40, "dehaze": 25}), dehaze + texture),
                (json!({"texture": 0, "dehaze": 25}), 0),
                // Texture's input moves while it is neutral.
                (json!({"texture": 0, "dehaze": 60}), moved),
                (json!({"texture": 20, "dehaze": 60}), texture),
                // Clarity has never run.
                (json!({"texture": 20, "clarity": 10, "dehaze": 60}), clarity),
                (json!({"texture": 20, "clarity": 0, "dehaze": 60}), 0),
                // Its planes still hold what this value reads.
                (json!({"texture": 20, "clarity": 10, "dehaze": 60}), 0),
                // The units after Dehaze read their input through its apply.
                (json!({"texture": 20, "clarity": 10}), texture + clarity),
            ],
        ),
        (
            "Dehaze across zero",
            vec![
                (json!({"texture": 40}), texture),
                (json!({"texture": 40, "dehaze": 30}), dehaze + texture),
                (json!({"texture": 40}), texture),
            ],
        ),
    ];
    for (drag, ticks) in drags {
        let plans: Vec<GpuPlan> = ticks
            .iter()
            .map(|(payload, _)| plan(payload, true))
            .collect();
        for (index, (payload, wanted)) in ticks.iter().enumerate() {
            let drawn: Vec<&GpuPlan> = plans[..=index].iter().collect();
            let (after, ran) = qualifier
                .evaluate_ticks(&drawn)
                .expect("a readback after the ticks before");
            let (whole, ran_whole) = qualifier
                .evaluate_ticks(&[&every(&plans[index])])
                .expect("a readback of every pass");
            eprintln!("{test}: {drag}: {payload}: {ran} of {all} passes");
            assert_eq!(ran_whole, all, "{drag}: {payload}: every pass");
            assert_eq!(ran, *wanted, "{drag}: {payload}");
            assert_eq!(
                differing(&after, &whole),
                0,
                "{drag}: {payload}: texels that differ from every pass run"
            );
            // Again with the poison on: every tick's passes start from NaN in every pool texture.
            qualifier.set_poison(true);
            let poisoned = qualifier.evaluate_ticks(&drawn);
            qualifier.set_poison(false);
            let (poisoned, _) = poisoned.expect("a poisoned readback after the ticks before");
            assert_eq!(
                differing(&poisoned, &whole),
                0,
                "{drag}: {payload}: texels that differ from every pass run, with the poison"
            );
        }
    }
}

/// A drafted layer's GPU shape — every unit, an amount-0 one its identity through its words — draws
/// what the CPU's shape of the same values draws: a layer at zero everywhere draws its input bit
/// for bit, and so does a unit that runs first in both shapes. A unit after an identity one reads
/// its input through that unit's apply, in pass and frame modules of their own, where Metal's fast
/// math compiles the same arithmetic a little differently: that difference is measured and held
/// far inside the spatial limits. At full resolution and at a Fit proxy's scale, on the linear
/// path, both shapes reading one light.
#[test]
fn gpu_presence_the_drafted_shape_draws_what_the_cpus_does() {
    let test = "gpu_presence_the_drafted_shape_draws_what_the_cpus_does";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let (width, height) = (480, 320);
    let pixels = photograph(width, height, 5);
    for request in [
        GpuPlanRequest::exact(0, stage(width, height)),
        GpuPlanRequest::fit(0, stage(width, height), stage(6000, 4000)),
    ] {
        let request = request.linear();
        let drafted = |payload: Value| {
            drafted_against_cpu(
                &qualifier,
                &recipe(payload),
                request,
                (width, height),
                &pixels,
            )
        };
        assert!(
            drafted(json!({})).input,
            "{request:?}: a neutral layer draws its input"
        );
        for (payload, first) in [
            (json!({"dehaze": 25}), true),
            (json!({"texture": 20, "dehaze": -40}), true),
            (json!({"texture": 40}), false),
            (json!({"clarity": -30}), false),
        ] {
            let drawn = drafted(payload.clone());
            eprintln!(
                "{test}: {} {payload}: {} of {} values differ, largest {:.2e}; codes by {}",
                if request.proxy { "proxy" } else { "full" },
                drawn.values,
                drawn.of,
                drawn.largest,
                drawn.code
            );
            if first {
                assert_eq!(drawn.values, 0, "{request:?}: {payload}");
            }
            assert!(
                drawn.largest <= 3.0e-5 && drawn.code <= 1,
                "{request:?}: {payload}: {drawn:?}"
            );
        }
    }
}

// Near-black outliers on the linear path, measured for a decision.
mod near_black;

// Detail followed by Presence, chained in one plan.
mod chain;

// Masked Presence layers chained in one plan, their scratch planes in one pool.
mod pool;

// As many masked spatial layers as a recipe may hold, of mixed shapes.
mod sixteen;

// What a masked Presence layer costs the GPU-preview budget, its scratch in the slot's one pool.
mod measured;

/// A masked Presence layer's passes run only over its mask's bounds grown by every unit's reach
/// (`GpuSpatial::pass_rect`), and its applies only where its coverage is not zero: the frame is the
/// one its passes give run over the whole boundary, which widening the mask's bounds to the whole
/// stage asks for — coverage outside the true bounds is exactly zero either way — bit for bit:
/// the reach covers each running sum's run back to where it starts over the whole plane, so no
/// sum the applies read carries a value an earlier pass left outside the rectangle. So it is with
/// the poison on, the scratch pool's textures holding NaN wherever the passes do not write them.
/// A small radial off centre, a radial at the edge and a linear gradient, at Fit's thin-feature
/// scale and at the exact stage, with Texture, Clarity and Dehaze, whose global estimate keeps the
/// whole boundary, and with Clarity alone.
#[test]
fn gpu_presence_a_masked_layer_runs_its_passes_over_its_mask_alone() {
    let test = "gpu_presence_a_masked_layer_runs_its_passes_over_its_mask_alone";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (720, 480);
    let pixels = photograph(width, height, 11);
    let held = boundary(width, height, 1, &pixels).expect("a boundary");
    let components = [
        luxforge_core::Component::new(
            "Radial 1",
            luxforge_core::ComponentMode::Add,
            "radial",
            json!({"x": 0.3, "y": 0.4, "radius_x": 0.12, "radius_y": 0.1, "angle": 25.0,
                   "feather": 40.0}),
        ),
        luxforge_core::Component::new(
            "Radial 2",
            luxforge_core::ComponentMode::Add,
            "radial",
            json!({"x": 0.97, "y": 0.9, "radius_x": 0.15, "radius_y": 0.12, "angle": 0.0,
                   "feather": 60.0}),
        ),
        luxforge_core::Component::new(
            "Linear 1",
            luxforge_core::ComponentMode::Add,
            "linear",
            json!({"x0": 0.6, "y0": 0.2, "x1": 0.85, "y1": 0.3}),
        ),
    ];
    let mut restricted = 0;
    for payload in [
        json!({"texture": 60, "clarity": 70, "dehaze": 30}),
        json!({"texture": 40, "clarity": 50}),
        json!({"clarity": -50}),
    ] {
        for component in &components {
            for request in [
                GpuPlanRequest::fit(0, stage(width, height), stage(width * 4, height * 4)),
                GpuPlanRequest::exact(0, stage(width, height)),
            ] {
                let stack = masked(recipe(payload.clone()), component);
                let plan = match gpu_plan(&registry, &stack, request).unwrap() {
                    GpuAnswer::Plan(plan) => *plan,
                    GpuAnswer::Fallback(reason) => panic!("{reason}"),
                };
                let bounded = surface_plan(&plan, held.clone()).expect("a runnable plan");
                let mut whole = bounded.clone();
                let GpuStep::Spatial(spatial) = &mut whole.steps[0] else {
                    panic!("a spatial step first");
                };
                let rect = spatial.pass_rect(bounded.texels, (width, height));
                let mask = spatial.mask.as_mut().expect("a masked step");
                // The mask's whole stage, where the plan addresses the doubled one under the
                // thin-feature supersample.
                mask.bounds = [0, 0, 4 * width, 4 * height];
                if rect != spatial.pass_rect(whole.texels, (width, height)) {
                    restricted += 1;
                }
                lit_fixed(&qualifier, &bounded);
                let (left, right) = (
                    qualifier.evaluate(&bounded).expect("a readback"),
                    qualifier.evaluate(&whole).expect("a readback"),
                );
                // Again with the pool's textures holding NaN outside what the passes write.
                qualifier.set_poison(true);
                let poisoned = qualifier.evaluate(&bounded).expect("a poisoned readback");
                qualifier.set_poison(false);
                let (differ, poisoned_differ) =
                    (differing(&left, &right), differing(&poisoned, &right));
                eprintln!(
                    "{test}: {payload} {} over {rect:?}: {differ} texels differ, {poisoned_differ} \
                     with the poison",
                    component.name
                );
                assert_eq!(
                    (differ, poisoned_differ),
                    (0, 0),
                    "{payload} {}",
                    component.name
                );
            }
        }
    }
    assert!(
        restricted > 0,
        "some plans ran their passes over less than the boundary"
    );
}

/// One, two and four masked Presence layers of Texture and Clarity, each through a radial of its
/// own, over a 24 MP photograph's full-screen Fit stage on a JPEG's half-float boundary, laid out
/// with their scratch planes in one pool (`chain_charge`): each layer is a link keeping the planes
/// its applies read, each link but the last writes an intermediate, and the pool holds one link's
/// scratch for all of them. A layer after the first adds its kept planes and an intermediate, and
/// the slot (`Qualifier::charged_bytes`, on a device, the figure the live slot charges) adds the
/// link's words and blocks buffers beside them: four layers charge 272.3 MB, where with each link
/// holding its scratch planes as its own they charged 495.5 MB.
#[test]
fn gpu_presence_masked_layers_take_their_scratch_from_one_pool() {
    use luxforge_ui::photo_surface::{BoundaryFormat, gpu_preview::chain_charge};
    let test = "gpu_presence_masked_layers_take_their_scratch_from_one_pool";
    let registry = ModuleRegistry::builtin();
    let (width, height) = (2292u32, 1528u32);
    let texels = Arc::new(vec![0u8; (width * height) as usize * 8]);
    let held =
        GpuBoundary::new(texels, width, height, 1, BoundaryFormat::Half).expect("a boundary");
    let plan_of = |layers: usize| {
        let mut stack = Recipe::default();
        for index in 0..layers {
            let mut mask = luxforge_core::Mask::new(format!("Mask {}", index + 1));
            mask.components.push(luxforge_core::Component::new(
                "Radial 1",
                luxforge_core::ComponentMode::Add,
                "radial",
                json!({"x": 0.2 + 0.2 * index as f64, "y": 0.5, "radius_x": 0.12,
                       "radius_y": 0.1, "angle": 0.0, "feather": 40.0}),
            ));
            stack.layers.push(Layer {
                mask: Some(mask.id.clone()),
                ..Layer::new(PRESENCE_EFFECT, json!({"texture": 40, "clarity": 30}))
            });
            stack.masks.push(mask);
        }
        let request = GpuPlanRequest::fit(0, stage(width, height), stage(6000, 4000));
        let plan = match gpu_plan(&registry, &stack, request).expect("the stack compiles") {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => panic!("{layers} layers: {reason}"),
        };
        // Each operation's planes as the core declares them, every one in a texture of its own.
        let planes: Vec<u64> = plan
            .spatial
            .iter()
            .map(|spatial| spatial.plane_bytes((0, 0), (width, height)))
            .collect();
        (
            surface_plan(&plan, held.clone()).expect("a runnable plan"),
            planes,
        )
    };
    // The figures over the boundary's 3,502,176 texels: an intermediate at eight bytes a texel;
    // each link's kept planes, 4.25 bytes a texel, and its 13 passes' parameter slices; and the
    // pool, one link's 21.25 bytes a texel of scratch.
    const INTERMEDIATE: u64 = 28_017_408;
    const KEPT: u64 = 14_884_248 + 13 * 256;
    const POOL: u64 = 74_421_240;
    // Each layer after the first adds its kept planes and an intermediate to the chain: 42.9 MB.
    const LAYER: u64 = 42_904_984;
    // The slot adds the link's words and blocks buffers too: 42.9 MB.
    const SLOT_LAYER: u64 = LAYER + 2 * 1024;
    let qualifier = crate::app::gpu_qualification::headless(test);
    // Each case: its layers, the chain's charge, the slot's, and the slot's when every link held
    // its scratch planes as its own.
    for (layers, chain, slot, own) in [
        (1usize, 89_308_816, 143_542_768, 143_542_768),
        (2, 132_213_800, 186_449_800, 260_871_040),
        (4, 218_023_768, 272_263_864, 495_527_584),
    ] {
        let (plan, planes) = plan_of(layers);
        let spatial = plan
            .steps
            .iter()
            .filter(|step| matches!(step, GpuStep::Spatial(_)))
            .count();
        assert_eq!(spatial, layers, "a spatial step a layer");
        // The pool holds one link's scratch: its planes less those its applies read.
        assert_eq!(planes, vec![KEPT - 13 * 256 + POOL; layers]);
        let origin = (
            plan.texels.origin[0].max(0.0) as u32,
            plan.texels.origin[1].max(0.0) as u32,
        );
        let charge = chain_charge(&plan.steps, (width, height), origin, BoundaryFormat::Half);
        eprintln!(
            "{test}: {layers} layers: intermediates {:?}, kept {:?}, pool {} B, chain {} B",
            charge.intermediates,
            charge.kept,
            charge.pool,
            charge.total()
        );
        assert_eq!(charge.intermediates, vec![INTERMEDIATE; layers - 1]);
        assert_eq!(charge.kept, vec![KEPT; layers]);
        assert_eq!(charge.pool, POOL);
        assert_eq!(charge.total(), chain);
        let after = layers as u64 - 1;
        assert_eq!(chain, KEPT + POOL + after * LAYER);
        assert_eq!(LAYER, KEPT + INTERMEDIATE);
        // The slot on a device, where its boundary, output and buffers are known.
        if let Some(qualifier) = &qualifier {
            let charged = qualifier.charged_bytes(&plan).expect("a charge");
            eprintln!("{test}: {layers} layers: the slot {charged} B");
            assert_eq!(charged, slot);
            assert_eq!(slot - 143_542_768, after * SLOT_LAYER);
            // Each link holding its own scratch charged the pool again for every layer after the
            // first.
            assert_eq!(own - slot, after * POOL);
        }
    }
}
