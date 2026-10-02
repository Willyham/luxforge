//! The Detail program on the GPU (`docs/design/gpu-preview.md`, "Spatial programs"), held to its
//! CPU kernels and units on this host's device.
//!
//! - The core's program passes the surface's own spatial convention, and a Detail plan converts
//!   into one spatial step, at full resolution and at a proxy's scale, masked or not.
//! - **Per kernel**, on synthetic planes: each kernel of `detail.wgsl` run through the surface's own
//!   assembled passes against the CPU kernel it transcribes (`luxforge_core::qualification`): the
//!   Oklab conversion, the separable smoothing (the B3 a-trous levels and sampled Gaussians, and
//!   sharpening's blur and guide together), one level's band and soft shrinkage, sharpening's
//!   coring, gating, extrema and limiter, and the reconstruction both applies end in. `tanh`, which
//!   the limiter takes, is measured on its own.
//! - **Per unit**, on synthetic photographs: noise reduction, sharpening and both, at full
//!   resolution and at a Fit proxy's scale, against the CPU's own units, judged by the spatial
//!   limits.
//!
//! A test with no adapter prints that it was skipped and asserts nothing: the skip is the report,
//! and `cargo test` counting it as passed does not make it GPU evidence.
use super::gpu_plan::surface_plan;
use super::gpu_qualification::{
    Stream, codes, corpus_at_fit, drafted_against_cpu, figures, grid, worst,
};
use luxforge_core::{
    Cancel, DETAIL_EFFECT, GPU_PROGRAMS, GpuAnswer, GpuPlanRequest, Layer, LinearImage,
    LinearSettings, ModuleRegistry, Recipe, RenderContext, RenderOptions, RenderSource, SnapshotId,
    SourceImage, Stage, gpu_plan, qualification::detail as cpu, qualification::detail::Smoothing,
    render,
};
use luxforge_reference::preview_error::{self, Class, Rgb8, Statistics};
use luxforge_ui::photo_surface::{
    GpuPlan, GpuProgram, GpuStep, TexelMap,
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

fn detail() -> &'static luxforge_core::GpuProgram {
    GPU_PROGRAMS
        .iter()
        .find(|program| program.entry == "lf_detail")
        .expect("the Detail program ships")
}

fn recipe(payload: Value) -> Recipe {
    Recipe {
        layers: vec![Layer::new(DETAIL_EFFECT, payload)],
        ..Recipe::default()
    }
}

/// The study's parameter sets (`fixtures/detail/corpus.json`), which the corpus runs too.
fn moderate() -> Value {
    json!({"luminance": 40, "colour": 40, "sharpening": 50, "radius": 1.0})
}

fn noise_stress() -> Value {
    json!({"luminance": 100, "colour": 100, "luminance-detail": 0, "colour-detail": 0})
}

fn sharpen_stress() -> Value {
    json!({"sharpening": 150, "radius": 3, "sharpen-detail": 100, "sharpen-masking": 0})
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

/// `recipe` with its Detail layer masked by one mask of `component`.
fn masked(mut recipe: Recipe, component: &luxforge_core::Component) -> Recipe {
    let mut mask = luxforge_core::Mask::new("Mask 1");
    mask.components.push(component.clone());
    recipe.layers[0].mask = Some(mask.id.clone());
    recipe.masks.push(mask);
    recipe
}

fn planned(answer: GpuAnswer) -> luxforge_core::GpuPlan {
    match answer {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("{reason}"),
    }
}

// ---- Without a device -------------------------------------------------------------------------

/// Every shape a Detail layer plans to (noise reduction, sharpening and both; at full resolution,
/// at a Fit proxy's scale and on the linear path; masked) converts into one spatial step that
/// passes the surface's own spatial convention. The program is enabled: it met the spatial limits
/// on the corpus at Fit against the CPU's moving proxy it stands in for.
#[test]
fn gpu_detail_the_program_passes_the_surfaces_own_convention() {
    let registry = ModuleRegistry::builtin();
    let (width, height) = (64, 48);
    for payload in [
        moderate(),
        noise_stress(),
        sharpen_stress(),
        json!({"luminance": 30}),
        json!({"colour": 30}),
    ] {
        for stack in [
            recipe(payload.clone()),
            masked(recipe(payload.clone()), &radial()),
        ] {
            for request in [
                GpuPlanRequest::exact(0, stage(width, height)),
                GpuPlanRequest::fit(0, stage(width, height), stage(220, 165)),
                GpuPlanRequest::exact(0, stage(width, height)).linear(),
            ] {
                let plan = planned(gpu_plan(&registry, &stack, request).unwrap());
                let held = boundary(width, height, 1, &vec![[0.25; 3]; 64 * 48]).unwrap();
                let converted = surface_plan(&plan, held).expect("a runnable plan");
                assert_eq!(converted.steps.len(), 1, "{payload}");
                let GpuStep::Spatial(step) = &converted.steps[0] else {
                    panic!("a spatial step");
                };
                assert_eq!(step.mask.is_some(), !stack.masks.is_empty());
                validate_step(&converted.steps[0])
                    .unwrap_or_else(|error| panic!("{payload}: {error}"));
            }
        }
    }
    assert!(
        detail().enabled,
        "Detail met the spatial limits on the corpus"
    );
}

// ---- Per kernel -------------------------------------------------------------------------------

/// The kernels a per-kernel test adds to the core's program: loaders of the boundary's channels
/// into planes, a probe of `tanh`, and an apply that shows a plane, each named after the program.
const TEST_KERNELS: &str = "
// The boundary at `at` moved by words 0 and 1, edge-clamped, times word 2.
fn lf_detail_test_load(at: vec2<i32>, words: u32, block: u32) {
    let offset = vec2<i32>(bitcast<i32>(lf_word(words)), bitcast<i32>(lf_word(words + 1u)));
    lf_store(at, vec4<f32>(lf_source(at + offset) * lf_f32(words + 2u), 0.0));
}

// The boundary's green and blue at `at`, as x and y.
fn lf_detail_test_load_yz(at: vec2<i32>, words: u32, block: u32) {
    let rgb = lf_source(at);
    lf_store(at, vec4<f32>(rgb.y, rgb.z, 0.0, 0.0));
}

// tanh of the texel's argument: its index in the first half of the plane times 2^-11 (0 to 16),
// in the second half times 2^-20 (0 to 1 / 32), and of its negation.
fn lf_detail_test_tanh(at: vec2<i32>, words: u32, block: u32) {
    let index = at.y * lf_size().x + at.x;
    let half = lf_size().x * lf_size().y / 2;
    var x = f32(index) * (1.0 / 2048.0);
    if index >= half {
        x = f32(index - half) * (1.0 / 1048576.0);
    }
    lf_store(at, vec4<f32>(tanh(x), tanh(-x), x, 0.0));
}

fn lf_detail_test_show(rgb: vec3<f32>, at: vec2<i32>, words: u32, block: u32, planes: u32) -> vec3<f32> {
    return lf_plane(planes, at).xyz;
}
";

fn test_program(words: Vec<u32>) -> GpuProgram {
    GpuProgram {
        words,
        ..GpuProgram::new("lf_detail", format!("{}{TEST_KERNELS}", detail().source))
    }
}

fn full(format: PlaneFormat) -> GpuPlane {
    GpuPlane {
        format,
        size: PlaneSize::Reduced(1),
    }
}

const EACH: PassShape = PassShape::Texels { span: [1, 1] };

fn pass(kernel: &'static str, inputs: &[u32], output: u32, words: u32) -> GpuPass {
    GpuPass {
        kernel: Cow::Borrowed(kernel),
        inputs: inputs.to_vec(),
        output,
        words,
        source: 0,
        shape: EACH,
    }
}

fn show(plane: u32) -> GpuApply {
    GpuApply {
        function: Cow::Borrowed("lf_detail_test_show"),
        planes: vec![plane],
        words: 0,
    }
}

/// The smoothing passes' forms, as `detail.wgsl` names them.
const FORM_EVERY: u32 = 0;
const FORM_PAIR: u32 = 1;

/// Words of the operation, each pass's first word recorded as it is appended.
#[derive(Default)]
struct Words(Vec<u32>);

impl Words {
    fn push(&mut self, values: &[u32]) -> u32 {
        let at = self.0.len() as u32;
        self.0.extend_from_slice(values);
        at
    }

    /// A load of the boundary moved by `(dx, dy)` and scaled by `scale`.
    fn load(&mut self, dx: i32, dy: i32, scale: f32) -> u32 {
        self.push(&[dx.cast_unsigned(), dy.cast_unsigned(), scale.to_bits()])
    }

    /// A smoothing pass of `form` over `kernels`, laid out as the descriptions lay theirs.
    fn smoothing(&mut self, form: u32, kernels: &[Smoothing]) -> u32 {
        let mut words = vec![form];
        for kernel in kernels {
            words.extend(cpu::slot_words(*kernel));
        }
        self.push(&words)
    }
}

/// A plan of one spatial step over a boundary of `values`, its program the core's text with the
/// test kernels, read back as `f32`.
fn run_step(
    qualifier: &Qualifier,
    (width, height): (u32, u32),
    values: &[[f32; 3]],
    words: Words,
    planes: Vec<GpuPlane>,
    passes: Vec<GpuPass>,
    apply: GpuApply,
) -> Vec<[f32; 4]> {
    let spatial = GpuSpatial {
        program: test_program(words.0),
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
        region: None,
    };
    qualifier.evaluate(&plan).expect("a qualification readback")
}

/// Synthetic Oklab-like planes: lightness in `[0, 1]` and chroma within about `±0.25`, with a
/// ramp, a step edge, a fine grating and noise, so windows see flat, ramped and busy
/// neighbourhoods. Each value as the boundary holds it.
fn lab_planes(width: u32, height: u32, seed: u64, noise: f32) -> Vec<[f32; 3]> {
    let mut stream = Stream(seed);
    (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let edge = if x > width / 3 && y < height * 2 / 3 {
                0.3
            } else {
                0.0
            };
            let grating = if x > width * 3 / 4 {
                0.08 * (x as f32 * 2.1).sin()
            } else {
                0.0
            };
            let mut n = || (stream.unit() as f32 - 0.5) * 2.0 * noise;
            let l = (0.2 + 0.5 * u + edge + grating + n()).clamp(0.0, 1.0);
            let a = 0.15 * (v - 0.5) + 0.1 * edge + n();
            let b = -0.12 * (u - 0.4) + n();
            [held(l), held(a), held(b)]
        })
        .collect()
}

/// One channel of `values`.
fn channel(values: &[[f32; 3]], c: usize) -> Vec<f32> {
    values.iter().map(|value| value[c]).collect()
}

/// The largest absolute difference over the texels both hold.
fn largest(gpu: &[f32], cpu: &[f32]) -> f64 {
    gpu.iter()
        .zip(cpu)
        .map(|(g, c)| f64::from((g - c).abs()))
        .fold(0.0, f64::max)
}

/// The Oklab pass over the extreme grid (in range, past white, below black and a half float's
/// extremes) against the CPU's conversion: within a few `f32` roundings in range, and finite
/// wherever the CPU is.
#[test]
fn gpu_detail_oklab_matches_the_cpu() {
    let test = "gpu_detail_oklab_matches_the_cpu";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let (width, height) = (256, 256);
    let values = grid(width, height, 0x0dd1);
    let mut words = Words::default();
    let w = words.push(&[0]);
    let gpu = run_step(
        &qualifier,
        (width, height),
        &values,
        words,
        vec![full(PlaneFormat::Quad)],
        vec![pass("lf_detail_lab", &[], 0, w)],
        show(0),
    );
    // Each difference relative to the CPU's value or one, whichever is larger: lightness grows as
    // the cube root of the input, to 2.5 at 16.
    let (mut in_range, mut past, mut non_finite) = (0.0_f64, 0.0_f64, 0);
    let mut at = [0.0_f32; 3];
    for (rgb, texel) in values.iter().zip(&gpu) {
        let lab = cpu::oklab(*rgb);
        for c in 0..3 {
            if lab[c].is_finite() && !texel[c].is_finite() {
                non_finite += 1;
            }
            let difference =
                f64::from((texel[c] - lab[c]).abs()) / f64::from(lab[c].abs()).max(1.0);
            if rgb.iter().all(|v| (-1.0..=16.0).contains(v)) {
                if difference > in_range {
                    at = *rgb;
                }
                in_range = in_range.max(difference);
            } else {
                past = past.max(difference);
            }
        }
    }
    eprintln!(
        "{test}: in [-1, 16] largest {in_range:.3e} (at {at:?}); past it {past:.3e}; non-finite \
         {non_finite}"
    );
    assert_eq!(non_finite, 0);
    // The largest is where an extended colour's LMS component cancels to near zero, where the
    // cube root's slope magnifies the one rounding the backend's fused multiply-adds change.
    assert!(
        in_range < 5.0e-5,
        "the Oklab conversion differs by {in_range:.3e}"
    );
}

/// The separable smoothing of every channel, the horizontal pass then the vertical, against the
/// CPU's: the B3 a-trous kernel at every level's spacing, and sampled Gaussians from below a
/// tenth of a pixel to the widest a proxy level reaches, alike and different on the two axes.
#[test]
fn gpu_detail_smoothing_matches_the_cpu_kernels() {
    let test = "gpu_detail_smoothing_matches_the_cpu_kernels";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let (width, height) = (224, 160);
    let values = lab_planes(width, height, 0x5100, 0.05);
    let mut cases: Vec<(Smoothing, Smoothing)> = [1, 2, 4, 8]
        .map(|spacing| (Smoothing::B3(spacing), Smoothing::B3(spacing)))
        .to_vec();
    for sigma in [0.05, 0.29, 0.5, 1.0, 2.3, 3.0, 7.9] {
        cases.push((Smoothing::Gaussian(sigma), Smoothing::Gaussian(sigma)));
    }
    cases.push((
        Smoothing::Gaussian(2.0 * 0.2860),
        Smoothing::Gaussian(2.0 * 0.2861),
    ));
    cases.push((Smoothing::Gaussian(0.6), Smoothing::Gaussian(4.1)));
    let mut worst_case = 0.0_f64;
    for (x, y) in cases {
        let mut words = Words::default();
        let load = words.load(0, 0, 1.0);
        let across = words.smoothing(FORM_EVERY, &[x]);
        let down = words.smoothing(FORM_EVERY, &[y]);
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
                pass("lf_detail_test_load", &[], 0, load),
                pass("lf_detail_smooth_x", &[0], 1, across),
                pass("lf_detail_smooth_y", &[1], 2, down),
            ],
            show(2),
        );
        let difference = (0..3)
            .map(|c| {
                let cpu = cpu::smooth(width, height, &channel(&values, c), x, y);
                let gpu: Vec<f32> = gpu.iter().map(|texel| texel[c]).collect();
                largest(&gpu, &cpu)
            })
            .fold(0.0, f64::max);
        eprintln!(
            "smoothing {x:?} ({} taps) then {y:?} ({} taps): largest {difference:.3e}",
            x.taps(),
            y.taps()
        );
        worst_case = worst_case.max(difference);
    }
    eprintln!("{test}: worst {worst_case:.3e}");
    assert!(
        worst_case < 1.0e-6,
        "the smoothing differs by {worst_case:.3e}"
    );
}

/// Sharpening's blur and guide of the lightness, smoothed together by the pair form, against the
/// CPU's two smoothings: every radius's ends at full resolution and at proxy scales.
#[test]
fn gpu_detail_blur_and_guide_match_the_cpu_kernels() {
    let test = "gpu_detail_blur_and_guide_match_the_cpu_kernels";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    let (width, height) = (200, 136);
    let values = lab_planes(width, height, 0x5200, 0.05);
    let lightness = channel(&values, 0);
    let mut worst_case = 0.0_f64;
    for (radius, sx, sy) in [
        (1.0, 1.0, 1.0),
        (0.5, 1.0, 1.0),
        (3.0, 1.0, 1.0),
        (1.0, 0.29, 0.29),
        (3.0, 0.1716, 0.1717),
    ] {
        let blur = [
            Smoothing::Gaussian(radius * sx),
            Smoothing::Gaussian(radius * sy),
        ];
        let guide = [Smoothing::Gaussian(sx), Smoothing::Gaussian(sy)];
        let mut words = Words::default();
        let load = words.load(0, 0, 1.0);
        let across = words.smoothing(FORM_PAIR, &[blur[0], guide[0]]);
        let down = words.smoothing(FORM_PAIR, &[blur[1], guide[1]]);
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
                pass("lf_detail_test_load", &[], 0, load),
                pass("lf_detail_smooth_x", &[0], 1, across),
                pass("lf_detail_smooth_y", &[1], 2, down),
            ],
            show(2),
        );
        let blurred = cpu::smooth(width, height, &lightness, blur[0], blur[1]);
        let guided = cpu::smooth(width, height, &lightness, guide[0], guide[1]);
        let difference = largest(&gpu.iter().map(|t| t[0]).collect::<Vec<_>>(), &blurred).max(
            largest(&gpu.iter().map(|t| t[1]).collect::<Vec<_>>(), &guided),
        );
        eprintln!("blur and guide, radius {radius} at ({sx}, {sy}): largest {difference:.3e}");
        worst_case = worst_case.max(difference);
    }
    eprintln!("{test}: worst {worst_case:.3e}");
    assert!(
        worst_case < 1.0e-6,
        "the blur and guide differ by {worst_case:.3e}"
    );
}

/// One wavelet level's band, its 3 x 3 energy and its soft shrinkage, added to an accumulated
/// change, against the CPU's: every level's thresholds at the moderate and stress strengths, the
/// detail settings' ends and middle, the first level and a later one, and one channel kind off.
/// The level's input is the boundary and its smoothing the boundary moved by a pixel, so bands
/// fall on both sides of every threshold.
#[test]
fn gpu_detail_shrinkage_matches_the_cpu() {
    let test = "gpu_detail_shrinkage_matches_the_cpu";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    let (width, height) = (192, 128);
    let values = lab_planes(width, height, 0x5300, 0.03);
    let at = |x: i64, y: i64| {
        let x = x.clamp(0, i64::from(width) - 1) as u32;
        let y = y.clamp(0, i64::from(height) - 1) as u32;
        values[(y * width + x) as usize]
    };
    let planar = |shift: (i64, i64), scale: f32| -> Vec<f32> {
        (0..3)
            .flat_map(|c| {
                (0..height).flat_map(move |y| {
                    (0..width)
                        .map(move |x| at(i64::from(x) + shift.0, i64::from(y) + shift.1)[c] * scale)
                })
            })
            .collect()
    };
    let coarse = planar((0, 0), 1.0);
    let next = planar((1, 0), 1.0);
    let accumulated = planar((-2, 1), 0.05);
    let band: Vec<f32> = coarse.iter().zip(&next).map(|(c, n)| c - n).collect();
    let len = (width * height) as usize;
    let mut worst_case = 0.0_f64;
    let mut cases = 0;
    for (luminance, colour) in [(40.0, 40.0), (100.0, 100.0), (60.0, 0.0), (0.0, 70.0)] {
        for (level, thresholds) in cpu::thresholds(luminance, colour).into_iter().enumerate() {
            for details in [[0.0_f32, 0.0], [0.5, 0.5], [1.0, 0.2]] {
                let first = level == 0;
                let mut words = Words::default();
                let load = words.load(0, 0, 1.0);
                let shifted = words.load(1, 0, 1.0);
                let before = words.load(-2, 1, 0.05);
                let shrink = words.push(&[
                    u32::from(!first),
                    thresholds[0].to_bits(),
                    thresholds[1].to_bits(),
                    details[0].to_bits(),
                    details[1].to_bits(),
                ]);
                let gpu = run_step(
                    &qualifier,
                    (width, height),
                    &values,
                    words,
                    vec![full(PlaneFormat::Quad); 4],
                    vec![
                        pass("lf_detail_test_load", &[], 0, load),
                        pass("lf_detail_test_load", &[], 1, shifted),
                        pass("lf_detail_test_load", &[], 2, before),
                        pass("lf_detail_shrink", &[0, 1, 2], 3, shrink),
                    ],
                    show(3),
                );
                let start = if first {
                    vec![0.0; 3 * len]
                } else {
                    accumulated.clone()
                };
                let cpu = cpu::shrink(width, height, &band, &start, thresholds, details);
                let difference = (0..3)
                    .map(|c| {
                        let gpu: Vec<f32> = gpu.iter().map(|texel| texel[c]).collect();
                        largest(&gpu, &cpu[c * len..(c + 1) * len])
                    })
                    .fold(0.0, f64::max);
                worst_case = worst_case.max(difference);
                cases += 1;
            }
        }
    }
    eprintln!("{test}: {cases} cases, worst {worst_case:.3e}");
    assert!(
        worst_case < 1.0e-6,
        "the shrinkage differs by {worst_case:.3e}"
    );
}

/// Sharpening's change of lightness, against the CPU's: the residual's coring, the guide's
/// gradient-energy gate, the gain, the 3 x 3 extrema and the `tanh` limiter, at the study's
/// settings, every coring and masking end and between. The lightness, its blur and its guide are
/// the boundary's three channels.
#[test]
fn gpu_detail_sharpening_matches_the_cpu() {
    let test = "gpu_detail_sharpening_matches_the_cpu";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    let (width, height) = (208, 144);
    let lab = lab_planes(width, height, 0x5400, 0.02);
    let mut stream = Stream(0x5401);
    // The lightness, a blur of it the residual is taken against, and a guide.
    let values: Vec<[f32; 3]> = lab
        .iter()
        .map(|[l, _, _]| {
            let blurred = l + (stream.unit() as f32 - 0.5) * 0.06;
            let guide = l + (stream.unit() as f32 - 0.5) * 0.01;
            [*l, held(blurred), held(guide)]
        })
        .collect();
    let (l, blurred, guide) = (
        channel(&values, 0),
        channel(&values, 1),
        channel(&values, 2),
    );
    let mut worst_case = 0.0_f64;
    let mut changed = 0usize;
    for (amount, detail, masking) in [
        (50.0, 25.0, 0.0),
        (150.0, 100.0, 0.0),
        (150.0, 0.0, 100.0),
        (60.0, 25.0, 50.0),
        (1.0, 50.0, 30.0),
    ] {
        let [gain, theta_squared, mask_squared] =
            cpu::sharpen_coefficients(amount, detail, masking);
        let mut words = Words::default();
        let load = words.load(0, 0, 1.0);
        let sharpen = words.push(&[
            gain.to_bits(),
            theta_squared.to_bits(),
            mask_squared.to_bits(),
        ]);
        let gpu = run_step(
            &qualifier,
            (width, height),
            &values,
            words,
            vec![
                full(PlaneFormat::Quad),
                full(PlaneFormat::Quad),
                full(PlaneFormat::Scalar),
            ],
            vec![
                pass("lf_detail_test_load", &[], 0, load),
                pass("lf_detail_test_load_yz", &[], 1, 0),
                pass("lf_detail_sharpen_l", &[0, 1], 2, sharpen),
            ],
            show(2),
        );
        let cpu = cpu::sharpen_change(width, height, &l, &blurred, &guide, amount, detail, masking);
        let gpu: Vec<f32> = gpu.iter().map(|texel| texel[0]).collect();
        let difference = largest(&gpu, &cpu);
        changed += cpu.iter().filter(|change| **change != 0.0).count();
        eprintln!("sharpen {amount} detail {detail} masking {masking}: largest {difference:.3e}");
        worst_case = worst_case.max(difference);
    }
    eprintln!("{test}: worst {worst_case:.3e} ({changed} changed pixels over the cases)");
    assert!(
        worst_case < 1.0e-6,
        "the change of lightness differs by {worst_case:.3e}"
    );
}

/// Both applies' reconstruction from Oklab against the CPU's: the input itself where the change
/// is zero; an exact grey, in range, where the CPU's changed chroma snaps to neutral; the changed
/// colour otherwise, within a few roundings in range; and finite wherever the CPU is, over the
/// extreme grid.
#[test]
fn gpu_detail_reconstruction_matches_the_cpu() {
    let test = "gpu_detail_reconstruction_matches_the_cpu";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    let (width, height) = (256, 256);
    // Each bound on the largest difference relative to the CPU's value or one: a photograph's
    // range, and the extreme grid's colours within [-1, 16], whose LMS components may cancel to
    // near zero, where the Oklab round trip magnifies a fused multiply-add's one rounding.
    for (name, values, bound) in [
        ("in range", lab_planes(width, height, 0x5500, 0.04), 1.0e-5),
        ("the extreme grid", grid(width, height, 0x5501), 1.0e-4),
    ] {
        let mut worst_case = 0.0_f64;
        // As linear RGB: the Oklab-like planes are read as colours here.
        for (apply, plane, scale) in [
            ("lf_detail_denoise", PlaneFormat::Quad, 0.02_f32),
            ("lf_detail_denoise", PlaneFormat::Quad, 0.0),
            ("lf_detail_sharpen", PlaneFormat::Scalar, 0.03),
            ("lf_detail_sharpen", PlaneFormat::Scalar, 0.0),
        ] {
            let mut words = Words::default();
            let load = words.load(3, -2, scale);
            let gpu = run_step(
                &qualifier,
                (width, height),
                &values,
                words,
                vec![full(plane)],
                vec![pass("lf_detail_test_load", &[], 0, load)],
                GpuApply {
                    function: Cow::Borrowed(apply),
                    planes: vec![0],
                    words: 0,
                },
            );
            let (mut difference, mut non_finite, mut changed_input, mut grey) = (0.0_f64, 0, 0, 0);
            for (index, (rgb, texel)) in values.iter().zip(&gpu).enumerate() {
                let (x, y) = (
                    index as i64 % i64::from(width),
                    index as i64 / i64::from(width),
                );
                let source = |x: i64, y: i64| {
                    let x = x.clamp(0, i64::from(width) - 1);
                    let y = y.clamp(0, i64::from(height) - 1);
                    values[(y * i64::from(width) + x) as usize]
                };
                let neighbour = source(x + 3, y - 2);
                let moved = neighbour.map(|value| value * scale);
                let delta = if apply == "lf_detail_sharpen" {
                    [moved[0], 0.0, 0.0]
                } else {
                    moved
                };
                let cpu = cpu::reconstruct(*rgb, cpu::oklab(*rgb), delta);
                let gpu = [texel[0], texel[1], texel[2]];
                if scale == 0.0 && gpu != *rgb {
                    changed_input += 1;
                }
                // The input and the change both in range: a half float's extremes moved into
                // Oklab are compared for finiteness alone.
                let in_range = rgb
                    .iter()
                    .chain(&neighbour)
                    .all(|v| (-1.0..=16.0).contains(v));
                if in_range
                    && cpu[0] == cpu[1]
                    && cpu[1] == cpu[2]
                    && !(gpu[0] == gpu[1] && gpu[1] == gpu[2])
                {
                    grey += 1;
                }
                for c in 0..3 {
                    if cpu[c].is_finite() && !gpu[c].is_finite() {
                        non_finite += 1;
                    }
                    if in_range && cpu[c].is_finite() {
                        let scale = f64::from(cpu[c].abs()).max(1.0);
                        difference = difference.max(f64::from((gpu[c] - cpu[c]).abs()) / scale);
                    }
                }
            }
            eprintln!(
                "{name}, {apply} at {scale}: largest {difference:.3e} relative, non-finite \
                 {non_finite}, input changed {changed_input}, greys not grey {grey}"
            );
            assert_eq!(
                (non_finite, changed_input, grey),
                (0, 0, 0),
                "{name}, {apply}"
            );
            worst_case = worst_case.max(difference);
        }
        eprintln!("{test}: {name}, worst {worst_case:.3e}");
        assert!(
            worst_case < bound,
            "the reconstruction differs by {worst_case:.3e} over {name}"
        );
    }
}

/// `tanh`, which the limiter takes, under this backend's compile options, against `f64` of the
/// same `f32` arguments: from 0 to 16 (past which the program holds the argument) and below 1/32,
/// where an implementation through `exp` loses relative precision. Reported, not judged: the
/// limiter scales it by 4% of the neighbourhood's range.
#[test]
fn gpu_detail_tanh_precision_is_measured() {
    let test = "gpu_detail_tanh_precision_is_measured";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let (width, height) = (256, 256);
    let mut words = Words::default();
    let w = words.push(&[0]);
    let gpu = run_step(
        &qualifier,
        (width, height),
        &vec![[0.0; 3]; (width * height) as usize],
        words,
        vec![full(PlaneFormat::Quad)],
        vec![pass("lf_detail_test_tanh", &[], 0, w)],
        show(0),
    );
    let half = gpu.len() / 2;
    let mut non_finite = 0;
    for (name, range) in [("[0, 16]", 0..half), ("[0, 1/32)", half..gpu.len())] {
        let (mut ulps, mut relative, mut absolute) = (0.0_f64, 0.0_f64, 0.0_f64);
        for texel in &gpu[range] {
            let x = f64::from(texel[2]);
            for (value, expected) in [(texel[0], x.tanh()), (texel[1], (-x).tanh())] {
                if !value.is_finite() {
                    non_finite += 1;
                    continue;
                }
                let error = (f64::from(value) - expected).abs();
                let ulp = f64::from((expected as f32).abs().next_up() - (expected as f32).abs())
                    .max(f64::from(f32::MIN_POSITIVE));
                ulps = ulps.max(error / ulp);
                absolute = absolute.max(error);
                if expected != 0.0 {
                    relative = relative.max(error / expected.abs());
                }
            }
        }
        eprintln!(
            "tanh(x), x in {name}: max {ulps:.1} ulp, relative {relative:.2e}, absolute \
             {absolute:.2e}"
        );
    }
    assert_eq!(non_finite, 0, "tanh is finite over the limiter's arguments");
}

// ---- Per unit ---------------------------------------------------------------------------------

/// A synthetic photograph for the units, in linear light: flat patches at four levels under
/// signal-dependent noise, a slanted edge, a fine grating, correlated chroma blotches and a
/// neutral ramp.
fn photograph(width: u32, height: u32, seed: u64) -> Vec<[f32; 3]> {
    let mut stream = Stream(seed);
    let mut gaussian = move || {
        let (u, v) = (stream.unit().max(f64::MIN_POSITIVE), stream.unit());
        ((-2.0 * u.ln()).sqrt() * (2.0 * std::f64::consts::PI * v).cos()) as f32
    };
    (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let base: [f32; 3] = if v < 0.5 {
                // Four flat patches across the top half.
                let level = [0.01, 0.05, 0.18, 0.45][(u * 4.0) as usize % 4];
                [level, level, level]
            } else if u < 0.33 {
                // A slanted edge.
                let edge =
                    (x as f32 - width as f32 * 0.16 - (y as f32 - height as f32 * 0.75) * 0.09)
                        .tanh();
                [0.1 + 0.2 * (edge + 1.0); 3]
            } else if u < 0.66 {
                // A fine grating over warm and cool blotches.
                let grating = 0.08 * (x as f32 * 2.3).sin();
                let blotch = 0.06 * ((x as f32 / 9.0).sin() * (y as f32 / 7.0).cos());
                [
                    0.3 + grating + blotch,
                    0.25 + grating,
                    0.2 + grating - blotch,
                ]
            } else {
                // A neutral ramp.
                [0.02 + 0.6 * (u - 0.66) / 0.34; 3]
            };
            base.map(|level| {
                let sigma = (4.0e-4 * level + 2.0e-6).sqrt();
                (level + sigma * gaussian()).max(0.0)
            })
        })
        .map(|rgb| rgb.map(held))
        .collect()
}

/// What one unit case measured.
struct Measured {
    program: Statistics,
    drawn: Statistics,
    linear: f64,
    non_finite: usize,
    passed: bool,
}

/// `stack` over `pixels` on the linear path, against the CPU's frame: at full resolution, the
/// render's exact frame (masks included); at a proxy's scale (`full` larger than the stage), the
/// CPU's units compiled at that scale over the whole frame.
fn measure_unit(
    qualifier: &Qualifier,
    registry: &ModuleRegistry,
    stack: &Recipe,
    (width, height): (u32, u32),
    full: (u32, u32),
    pixels: &[[f32; 3]],
) -> Measured {
    let exact = full == (width, height);
    let cpu: Vec<[f32; 3]> = if exact {
        let len = pixels.len();
        let mut planes = vec![0.0_f32; 3 * len];
        for (index, pixel) in pixels.iter().enumerate() {
            for c in 0..3 {
                planes[c * len + index] = pixel[c];
            }
        }
        let image = LinearImage::with_fingerprint(width, height, planes, "sha256:gpu-detail")
            .expect("a linear image");
        let source = RenderSource::Linear {
            image: &image,
            settings: LinearSettings::default(),
        };
        let frame = render(
            registry,
            source,
            stack,
            RenderOptions::exact(&Cancel::never()),
            &RenderContext::new(),
        )
        .and_then(|render| render.frame(SnapshotId::new()))
        .expect("the CPU frame");
        let table = luxforge_core::colour::srgb::decode_table();
        frame
            .rgba
            .chunks_exact(4)
            .map(|p| [0, 1, 2].map(|c| table[usize::from(p[c])]))
            .collect()
    } else {
        assert!(
            stack.masks.is_empty(),
            "a proxy's units are measured unmasked"
        );
        cpu::detail(width, height, full, pixels, &stack.layers[0].payload)
    };
    let request = if exact {
        GpuPlanRequest::exact(0, stage(width, height))
    } else {
        GpuPlanRequest::fit(0, stage(width, height), stage(full.0, full.1))
    };
    let plan = planned(gpu_plan(registry, stack, request.qualifying().linear()).unwrap());
    let converted =
        surface_plan(&plan, boundary(width, height, 1, pixels).unwrap()).expect("runnable");
    let gpu = qualifier.evaluate(&converted).expect("a readback");
    let drawn = qualifier.evaluate_codes(&converted).expect("a readback");
    let non_finite = gpu
        .iter()
        .flat_map(|texel| texel[..3].iter())
        .filter(|value| !value.is_finite())
        .count();
    let reference = codes(cpu.iter().copied());
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
    // On the exact path the CPU frame is 8-bit, so only the proxy's float output is compared in
    // linear light.
    let linear = if exact {
        0.0
    } else {
        gpu.iter()
            .zip(&cpu)
            .flat_map(|(g, c)| (0..3).map(move |i| f64::from((g[i] - c[i]).abs())))
            .fold(0.0, f64::max)
    };
    Measured {
        passed: preview_error::verdict(&program, Class::Spatial).passed() && non_finite == 0,
        drawn: compare(&candidate),
        program,
        linear,
        non_finite,
    }
}

/// Noise reduction, sharpening and both, at the study's settings and each alone, over a synthetic
/// photograph at two sizes, against the CPU: at full resolution (the B3 a-trous levels), unmasked
/// and masked by a feathered radial, and at a Fit proxy's scale (sampled Gaussians). The program's
/// `f32` output through the reference quantizer is held to the spatial limits and the finiteness
/// rule; the drawn codes are reported beside it.
#[test]
fn gpu_detail_units_meet_the_spatial_limits() {
    let test = "gpu_detail_units_meet_the_spatial_limits";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let registry = ModuleRegistry::builtin();
    let cases = [
        moderate(),
        noise_stress(),
        sharpen_stress(),
        json!({"luminance": 60}),
        json!({"colour": 70, "colour-detail": 20}),
        json!({"sharpening": 80, "radius": 0.5, "sharpen-masking": 60}),
    ];
    let (mut programs, mut drawn, mut missed) = (Vec::new(), Vec::new(), Vec::new());
    let mut linear = 0.0_f64;
    for ((width, height), proxy) in [((480, 320), (1655, 1103)), ((1536, 1024), (6000, 4000))] {
        let pixels = photograph(width, height, u64::from(width));
        for payload in &cases {
            for (stack, full, label) in [
                (recipe(payload.clone()), (width, height), "full resolution"),
                (
                    masked(recipe(payload.clone()), &radial()),
                    (width, height),
                    "full resolution, masked",
                ),
                (recipe(payload.clone()), proxy, "proxy"),
            ] {
                let measured = measure_unit(
                    &qualifier,
                    &registry,
                    &stack,
                    (width, height),
                    full,
                    &pixels,
                );
                let name = format!("{payload} at {width}x{height}, {label}");
                eprintln!(
                    "{name}: program {} | drawn {} | linear {:.2e} | non-finite {}{}",
                    figures(&measured.program),
                    figures(&measured.drawn),
                    measured.linear,
                    measured.non_finite,
                    if measured.passed { "" } else { " MISS" }
                );
                linear = linear.max(measured.linear);
                programs.push(measured.program);
                drawn.push(measured.drawn);
                if !measured.passed {
                    missed.push(name);
                }
            }
        }
    }
    eprintln!(
        "{test}: worst of {} cases: program {} | drawn {} | proxy linear {linear:.2e}",
        programs.len(),
        figures(&worst(&programs)),
        figures(&worst(&drawn))
    );
    assert!(
        missed.is_empty(),
        "cases missing the spatial limits: {missed:?}"
    );
}

/// The study's settings on the byte path, over an 8-bit source at full resolution: the
/// operation's input and output are clamped where the CPU quantizes its frames.
#[test]
fn gpu_detail_on_the_byte_path_meets_the_spatial_limits() {
    let test = "gpu_detail_on_the_byte_path_meets_the_spatial_limits";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (960, 640);
    let pixels = photograph(width, height, 9);
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
        fingerprint: "sha256:gpu-detail-bytes".into(),
        orientation: 1,
        capture: Arc::default(),
    };
    let table = luxforge_core::colour::srgb::decode_table();
    let texels: Vec<[f32; 3]> = rgba
        .chunks_exact(4)
        .map(|pixel| [0, 1, 2].map(|c| table[usize::from(pixel[c])]))
        .collect();
    let (mut statistics, mut missed) = (Vec::new(), Vec::new());
    for payload in [moderate(), noise_stress(), sharpen_stress()] {
        let stack = recipe(payload.clone());
        let cpu = render(
            &registry,
            &image,
            &stack,
            RenderOptions::exact(&Cancel::never()),
            &RenderContext::new(),
        )
        .and_then(|render| render.frame(SnapshotId::new()))
        .expect("the CPU frame");
        let plan = planned(
            gpu_plan(
                &registry,
                &stack,
                GpuPlanRequest::exact(0, stage(width, height)).qualifying(),
            )
            .unwrap(),
        );
        assert!(plan.spatial.as_ref().unwrap().clamps);
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

/// Detail's pass pipelines are its kernels and shapes: both units run 17 passes from 6 pipelines,
/// and a drag of any slider that keeps the units' structure, which changes only words, compiles
/// none.
#[test]
fn gpu_detail_pass_pipelines_are_shared() {
    let test = "gpu_detail_pass_pipelines_are_shared";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let (width, height) = (320, 240);
    let pixels = photograph(width, height, 5);
    let converted = |payload: Value| {
        let plan = planned(
            gpu_plan(
                &registry,
                &recipe(payload),
                GpuPlanRequest::fit(0, stage(width, height), stage(1200, 900))
                    .qualifying()
                    .linear(),
            )
            .unwrap(),
        );
        let passes = plan
            .spatial
            .as_ref()
            .map_or(0, |spatial| spatial.passes.len());
        (
            surface_plan(&plan, boundary(width, height, 1, &pixels).unwrap()).unwrap(),
            passes,
        )
    };
    let before = qualifier.pass_pipelines_created();
    let (plan, passes) = converted(moderate());
    qualifier.evaluate(&plan).expect("a readback");
    let first = qualifier.pass_pipelines_created() - before;
    eprintln!("{test}: moderate: {passes} passes, {first} pipelines");
    assert_eq!((passes, first), (17, 6));
    for payload in [
        json!({"luminance": 90, "colour": 5, "sharpening": 120, "radius": 2.7,
               "sharpen-detail": 80, "sharpen-masking": 40, "luminance-detail": 10,
               "colour-detail": 90}),
        json!({"luminance": 1, "colour": 100, "sharpening": 1, "radius": 0.5}),
    ] {
        qualifier
            .evaluate(&converted(payload).0)
            .expect("a readback");
    }
    assert_eq!(
        qualifier.pass_pipelines_created() - before,
        first,
        "a drag compiles nothing"
    );
}

/// What the photo surface's slot drawing both Detail units charges the GPU-preview budget at Fit:
/// the 60 MP JPEG's Fit stage, the evidence window's whole Fit bounds and a 3026 × 1826 region (the
/// largest 100% window measured) fit it; a stage past about 8 MP does not, so the surface refuses it
/// before creating anything and draws the CPU's frame, naming the budget
/// (`spatial_planes_are_charged_released_and_refused_past_the_budget`).
#[test]
fn gpu_detail_planes_are_charged_to_the_budget() {
    let test = "gpu_detail_planes_are_charged_to_the_budget";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    let registry = ModuleRegistry::builtin();
    let budget = luxforge_ui::photo_surface::GPU_PREVIEW_BUDGET;
    for ((width, height), full, fits) in [
        ((1716, 1030), (10000, 6000), true),
        ((1716, 1508), (4024, 3537), true),
        ((2400, 1600), (6000, 4000), true),
        ((3026, 1826), (10000, 6000), true),
        ((3464, 2309), (10000, 6667), true),
        ((4000, 2667), (10000, 6667), false),
    ] {
        let plan = planned(
            gpu_plan(
                &registry,
                &recipe(moderate()),
                GpuPlanRequest::fit(0, stage(width, height), stage(full.0, full.1)).qualifying(),
            )
            .unwrap(),
        );
        let planes = plan
            .spatial
            .as_ref()
            .unwrap()
            .plane_bytes((0, 0), (width, height));
        let pixels = vec![[0.25_f32; 3]; (width * height) as usize];
        let converted = surface_plan(&plan, boundary(width, height, 1, &pixels).unwrap()).unwrap();
        let charged = qualifier.charged_bytes(&converted).expect("a charge");
        eprintln!(
            "{test}: {width}x{height}: planes {planes} B ({:.1} a pixel), charged {charged} B \
             ({:.1} MiB of {} MiB)",
            planes as f64 / f64::from(width * height),
            charged as f64 / 1048576.0,
            budget / 1048576
        );
        assert_eq!(planes, 68 * u64::from(width * height));
        assert_eq!(charged <= budget, fits, "{width}x{height}");
    }
}

/// A drafted layer's GPU shape — both units and every level of noise reduction, one at zero its
/// identity through its words — draws what the CPU's shape of the same values draws: a layer at
/// zero everywhere draws its input bit for bit, and so does every stack whose noise reduction runs
/// in both shapes, its fourth level at zero included. Sharpening alone runs after noise reduction's
/// identity in the drafted shape and reads its input through that unit's apply, in pass and frame
/// modules of their own, where Metal's fast math compiles the same arithmetic a little differently:
/// that difference is measured and held far inside the spatial limits. At full resolution and at a
/// Fit proxy's scale, on the linear path.
#[test]
fn gpu_detail_the_drafted_shape_draws_what_the_cpus_does() {
    let test = "gpu_detail_the_drafted_shape_draws_what_the_cpus_does";
    let Some(qualifier) = crate::app::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let (width, height) = (480, 320);
    let pixels = photograph(width, height, 5);
    for request in [
        GpuPlanRequest::exact(0, stage(width, height)),
        GpuPlanRequest::fit(0, stage(width, height), stage(1655, 1103)),
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
            (json!({"luminance": 40}), true),
            (json!({"colour": 30}), true),
            (moderate(), true),
            (json!({"sharpening": 60}), false),
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

// ---- Drags at Fit -----------------------------------------------------------------------------

mod drags {
    //! A Detail drag at Fit drawn on the GPU, end to end against a real owner and preview worker,
    //! with what the surface reports stood in for, as `gpu_preview_tests` does.
    use super::super::{
        Editor, Message,
        gpu_preview::SurfaceReport,
        message::{preview::PreviewMessage, view::ViewMessage},
        testing::{attach_log, events, finish, let_go, logged, real_photo, run_commit, slide},
    };
    use luxforge_testbase::wait_until;
    use luxforge_ui::photo_surface::GpuStep;
    use serde_json::Value;

    fn catalog(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "luxforge-gpu-detail-{name}-{}.sqlite",
            std::process::id()
        ))
    }

    /// Take up the preview worker's results until `done`.
    fn deliver_until(editor: &mut Editor, what: &str, mut done: impl FnMut(&Editor) -> bool) {
        wait_until(what, || {
            let _ = editor.update(Message::Preview(PreviewMessage::Poll));
            done(editor)
        });
    }

    /// The surface reports that it evaluated the held boundary's plan with no fallback.
    fn surface_ready(editor: &mut Editor) {
        let version = editor.gpu.held_version().expect("a held boundary");
        editor.gpu.surface = Some(SurfaceReport {
            ready_boundary: Some(version),
            fallback: None,
            drawn: None,
        });
    }

    fn jobs(records: &[Value]) -> usize {
        events(records, "preview_job_requested").len()
    }

    /// The program of the spatial step the surface is handed.
    fn spatial_program(editor: &Editor) -> Option<String> {
        editor.surfaces().gpu.and_then(|plan| {
            plan.steps.iter().find_map(|step| match step {
                GpuStep::Spatial(spatial) => Some(spatial.program.entry.to_string()),
                _ => None,
            })
        })
    }

    /// A Detail Amount drag at Fit on a photograph drawn as a proxy. Its first tick takes the CPU
    /// path and asks for the boundary: the input of the restoration layer, which is the proxy
    /// source itself. Once the boundary is held and the surface has evaluated it, every tick is
    /// Detail's spatial step drawn on the GPU with no preview job. The release commits, and the
    /// CPU's frames replace the GPU's: the moving proxy, then the reduction of the exact render
    /// the stack settles to; the boundary goes once that is presented.
    #[test]
    fn gpu_detail_a_fit_drag_is_drawn_on_the_gpu_and_settles_from_exact() {
        let catalog = catalog("drag");
        let (mut editor, _, _) = real_photo(&catalog);
        // A window too small for the photograph at its own size: the Fit frame is a proxy, and
        // a Detail stack settles from the exact render's reduction.
        let _ = editor.update(Message::View(ViewMessage::Resized(900.0, 600.0)));
        editor.gpu.surface = Some(SurfaceReport::default());
        let log = attach_log(&mut editor);
        let _ = slide(&mut editor, "set-detail", "sharpening", 40.0);
        deliver_until(&mut editor, "the boundary", |editor| {
            editor.gpu.holds_boundary()
        });
        let records = logged(&mut editor, &log);
        let ticks = events(&records, "gpu_preview_tick");
        assert_eq!(ticks[0]["path"], "cpu");
        assert_eq!(ticks[0]["reason"], "boundary-pending");
        assert_eq!(ticks[0]["boundary_requested"], true);
        // The boundary is the Detail layer's input at the proxy's size.
        let summary = editor.gpu.summary();
        let boundary = &summary["drag"]["boundary"];
        assert_eq!(boundary["layer"], 0, "{summary}");
        let (width, height) = (
            boundary["width"].as_u64().unwrap(),
            boundary["height"].as_u64().unwrap(),
        );
        assert!(width < 480 && height < 320, "a proxy, {width}x{height}");
        assert_eq!(
            spatial_program(&editor).as_deref(),
            Some("lf_detail"),
            "Detail's spatial step is drawn"
        );
        surface_ready(&mut editor);
        let log = attach_log(&mut editor);
        for amount in [60.0, 80.0, 100.0] {
            let _ = slide(&mut editor, "set-detail", "sharpening", amount);
            let revision = editor.session.draft.as_ref().unwrap().draft_revision;
            let surfaces = editor.surfaces();
            assert!(surfaces.gpu.is_some(), "the plan is drawn");
            assert_eq!(surfaces.gpu_tag, Some(revision), "tagged with its tick");
        }
        let records = logged(&mut editor, &log);
        assert_eq!(jobs(&records), 0, "no preview job per tick");
        let ticks = events(&records, "gpu_preview_tick");
        assert_eq!(ticks.len(), 3);
        assert!(ticks.iter().all(|tick| tick["path"] == "gpu"));
        // The release commits. The CPU's frames take over: the moving proxy, then the reduction
        // of the exact render, after which the boundary is let go.
        let log = attach_log(&mut editor);
        let _ = let_go(&mut editor, "set-detail", "sharpening");
        assert!(run_commit(&mut editor));
        deliver_until(&mut editor, "the settled Fit frame", |editor| {
            !editor.gpu.has_drag() && editor.presentation.presented_settled
        });
        let records = logged(&mut editor, &log);
        let shown = events(&records, "preview_displayed");
        let settled = shown
            .iter()
            .position(|frame| frame["settled_from_exact"] == true)
            .expect("the exact-derived Fit frame");
        assert!(
            shown[..settled]
                .iter()
                .any(|frame| frame["proxy"] == true && frame["settled_from_exact"] == false),
            "the moving proxy before it: {shown:?}"
        );
        assert_eq!(
            events(&records, "gpu_boundary_released")[0]["why"],
            "draft-ended"
        );
        assert!(editor.surfaces().gpu.is_none());
        finish(editor, catalog);
    }

    /// A drag of a layer after a committed Detail layer: its boundary is the restoration prefix's
    /// output, which the worker reads from the restoration-prefix proxy cache its first tick's
    /// frame just held, and its ticks are drawn on the GPU with no preview job, while the CPU
    /// frame before them reports the cache's use.
    #[test]
    fn gpu_detail_a_drag_after_detail_starts_from_the_restoration_prefix() {
        let catalog = catalog("suffix");
        let (mut editor, _, _) = real_photo(&catalog);
        let _ = editor.update(Message::View(ViewMessage::Resized(900.0, 600.0)));
        let _ = slide(&mut editor, "set-detail", "sharpening", 60.0);
        let _ = let_go(&mut editor, "set-detail", "sharpening");
        assert!(run_commit(&mut editor));
        deliver_until(&mut editor, "the committed Detail frame", |editor| {
            !editor.gpu.has_drag() && editor.presentation.presented_settled
        });
        editor.gpu.surface = Some(SurfaceReport::default());
        let _ = slide(&mut editor, "set-basic", "exposure", 0.2);
        deliver_until(&mut editor, "the boundary", |editor| {
            editor.gpu.holds_boundary() && editor.presentation.restoration_prefix.is_some()
        });
        assert_eq!(editor.gpu.summary()["drag"]["boundary"]["layer"], 1);
        assert!(editor.presentation.restoration_prefix.is_some());
        // The plan runs from Basic's input: colour, no spatial step.
        assert!(editor.surfaces().gpu.is_some());
        assert_eq!(spatial_program(&editor), None);
        surface_ready(&mut editor);
        let log = attach_log(&mut editor);
        for exposure in [0.4, 0.6] {
            let _ = slide(&mut editor, "set-basic", "exposure", exposure);
        }
        let records = logged(&mut editor, &log);
        assert_eq!(jobs(&records), 0, "no preview job per tick");
        assert!(
            events(&records, "gpu_preview_tick")
                .iter()
                .all(|tick| tick["path"] == "gpu")
        );
        let _ = editor.update(Message::Draft(
            super::super::message::draft::DraftMessage::Cancel,
        ));
        finish(editor, catalog);
    }

    /// What the surface keys the spatial step's pipelines by: its clamp and mask, its planes, its
    /// passes and its applies, but for every word's value.
    fn spatial_shape(editor: &Editor) -> Option<String> {
        editor.surfaces().gpu.and_then(|plan| {
            plan.steps.iter().find_map(|step| match step {
                GpuStep::Spatial(spatial) => {
                    let passes: Vec<_> = spatial
                        .passes
                        .iter()
                        .map(|pass| {
                            let (kernel, inputs) = (&pass.kernel, &pass.inputs);
                            format!(
                                "{kernel} {inputs:?} {} {} {:?}",
                                pass.output, pass.source, pass.shape
                            )
                        })
                        .collect();
                    Some(format!(
                        "clamps {} masked {}: {:?} {passes:?} {:?}",
                        spatial.clamps,
                        spatial.mask.is_some(),
                        spatial.planes,
                        spatial.applies
                    ))
                }
                _ => None,
            })
        })
    }

    /// A drag of a Detail strength or a Presence field from zero, through values of both signs
    /// where the field has them, back to zero and off it again: every tick's spatial step has the
    /// shape of the first, so the surface draws all of them from one compiled sequence, and every
    /// tick after the boundary is held is drawn on the GPU with no preview job. Colour crossing
    /// zero adds and removes noise reduction's coarsest level on the CPU; Dehaze crossing it adds
    /// and removes a unit, and its first frame stores the atmospheric light the later ticks read.
    #[test]
    fn gpu_detail_and_presence_drags_across_zero_keep_one_sequence() {
        for (name, committed, action, field, values) in [
            (
                "colour",
                Some(("set-detail", "sharpening", 40.0)),
                "set-detail",
                "colour",
                [30.0, 60.0, 0.0, 25.0, 0.0],
            ),
            (
                "dehaze",
                None,
                "set-presence",
                "dehaze",
                [40.0, -30.0, 0.0, 20.0, 0.0],
            ),
        ] {
            let catalog = catalog(&format!("across-{name}"));
            let (mut editor, _, _) = real_photo(&catalog);
            let _ = editor.update(Message::View(ViewMessage::Resized(900.0, 600.0)));
            if let Some((action, field, value)) = committed {
                let _ = slide(&mut editor, action, field, value);
                let _ = let_go(&mut editor, action, field);
                assert!(run_commit(&mut editor));
                deliver_until(&mut editor, "the committed frame", |editor| {
                    !editor.gpu.has_drag() && editor.presentation.presented_settled
                });
            }
            editor.gpu.surface = Some(SurfaceReport::default());
            let _ = slide(&mut editor, action, field, values[0]);
            deliver_until(&mut editor, "the boundary", |editor| {
                editor.gpu.holds_boundary()
            });
            let first = spatial_shape(&editor).expect("the first tick's spatial step");
            surface_ready(&mut editor);
            let log = attach_log(&mut editor);
            for &value in &values[1..] {
                let _ = slide(&mut editor, action, field, value);
                assert_eq!(
                    spatial_shape(&editor).as_ref(),
                    Some(&first),
                    "{name} at {value}: one sequence"
                );
            }
            let records = logged(&mut editor, &log);
            let ticks = events(&records, "gpu_preview_tick");
            assert_eq!(ticks.len(), values.len() - 1, "{name}");
            assert!(
                ticks.iter().all(|tick| tick["path"] == "gpu"),
                "{name}: {ticks:?}"
            );
            assert_eq!(jobs(&records), 0, "{name}: no preview job per tick");
            let _ = editor.update(Message::Draft(
                super::super::message::draft::DraftMessage::Cancel,
            ));
            finish(editor, catalog);
        }
    }

    /// A Detail Amount drag at 100% is drawn over the visible region at full scale, here the whole
    /// 480 × 320 photograph, which the window shows at that zoom: its first tick takes the CPU path and its region job carries the one boundary request, the Detail layer's
    /// input over the window the region reads; once that is held and the surface has evaluated it,
    /// every tick is Detail's spatial step in its GPU shape, drawn on the GPU with no preview job
    /// and no region job.
    #[test]
    fn gpu_detail_a_drag_at_100_percent_is_drawn_on_the_gpu_with_no_job_per_tick() {
        let catalog = catalog("zoom");
        let (mut editor, _, _) = real_photo(&catalog);
        deliver_until(&mut editor, "the first frame", |editor| {
            editor.presentation.dimensions.is_some() && !editor.presentation.queue.is_busy()
        });
        editor.session.preview.view.zoom = luxforge_core::Zoom::Percent { value: 100.0 };
        let stage = editor
            .presentation
            .dimensions
            .expect("the photograph's stage");
        let wanted = editor.desired_view_for(stage).expect("a visible region");
        editor.gpu.surface = Some(SurfaceReport::default());
        let log = attach_log(&mut editor);
        let _ = slide(&mut editor, "set-detail", "sharpening", 40.0);
        deliver_until(&mut editor, "the region's boundary", |editor| {
            editor.gpu.holds_boundary()
        });
        let records = logged(&mut editor, &log);
        let ticks = events(&records, "gpu_preview_tick");
        assert_eq!(ticks[0]["path"], "cpu");
        assert_eq!(ticks[0]["boundary_requested"], true);
        let summary = editor.gpu.summary();
        assert_eq!(summary["drag"]["zoom"], 100.0, "{summary}");
        assert_eq!(summary["drag"]["boundary"]["layer"], 0, "{summary}");
        assert_eq!(
            summary["drag"]["shape"], "gpu",
            "every unit fits the budget"
        );
        surface_ready(&mut editor);
        let log = attach_log(&mut editor);
        for amount in [60.0, 80.0, 100.0] {
            let _ = slide(&mut editor, "set-detail", "sharpening", amount);
            let surfaces = editor.surfaces();
            let plan = surfaces.gpu.expect("the region's plan is drawn");
            let region = plan.region.expect("a region plan");
            assert_eq!(
                region.rect,
                [wanted.x0, wanted.y0, wanted.x1(), wanted.y1()]
            );
            assert_eq!(spatial_program(&editor).as_deref(), Some("lf_detail"));
            assert!(!editor.view_plan.in_flight, "no region job for the view");
        }
        let records = logged(&mut editor, &log);
        assert_eq!(jobs(&records), 0, "no preview job per tick");
        let ticks = events(&records, "gpu_preview_tick");
        assert_eq!(ticks.len(), 3);
        assert!(ticks.iter().all(|tick| tick["path"] == "gpu"), "{ticks:?}");
        let _ = editor.update(Message::Draft(
            super::super::message::draft::DraftMessage::Cancel,
        ));
        finish(editor, catalog);
    }
}

// ---- The corpus at Fit ------------------------------------------------------------------------

/// The qualification corpus's Detail recipes at Fit through the shared harness
/// ([`corpus_at_fit`]), each held to the spatial limits against the CPU's moving proxy the GPU
/// frame stands in for (owner, 2026-10-02), with the jump from that proxy to the exact-derived
/// frame a Detail stack settles to reported beside it.
///
/// ```sh
/// LUXFORGE_GPU_CORPUS_OUTPUT=/tmp/new-dir \
/// LUXFORGE_GENERATED_FIXTURES=fixtures/generated \
/// LUXFORGE_RAW_MANIFEST=/path/to/raw-manifest.json \
/// cargo test -p luxforge-app gpu_detail_corpus -- --ignored --nocapture
/// ```
#[test]
#[ignore = "the GPU preview corpus at Fit: set LUXFORGE_GPU_CORPUS_OUTPUT to a new directory, \
            LUXFORGE_GENERATED_FIXTURES to the generated JPEGs and, for the RAWs, \
            LUXFORGE_RAW_MANIFEST"]
fn gpu_detail_corpus_at_fit() {
    corpus_at_fit("gpu_detail_corpus_at_fit", &["detail"]);
}
