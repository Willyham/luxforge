//! The GPU programs of the seven pointwise colour units (`docs/design/gpu-preview.md`), qualified
//! against their CPU units on this host's device, and the conversion that hands their plans to the
//! photo surface.
//!
//! - Every program the core ships passes the surface's own calling convention, with the surface's
//!   own prelude, so the core's test copy of the prelude cannot drift from it unnoticed.
//! - The converter turns a colour plan into one colour step per unit and names what the surface
//!   cannot run yet.
//! - Each unit's program, run through the surface's own assembled shader over a dense synthetic
//!   grid (negative, in-range and over-range linear values, and every parameter extreme), meets
//!   the pointwise limits against its CPU unit, and never gives a non-finite value where the CPU
//!   gives a finite one.
//! - The precision of `pow`, `exp`, `exp2`, `cos`, `atan2` and the cube root the programs build from
//!   `pow`, under this backend's compile options, is measured rather than assumed.
//!
//! A test with no adapter prints that it was skipped and asserts nothing: the skip is the report,
//! and `cargo test` counting it as passed does not make it GPU evidence.
use super::gpu_plan::{Unrunnable, program, surface_plan};
use luxforge_core::{
    BASIC_EFFECT, CURVE_EFFECT, CompileStage, EFFECT_FORMAT, GPU_PROGRAMS, GpuAnswer,
    GpuPlanRequest, GpuProgramKind, Layer, MIXER_EFFECT, ModuleRegistry, PointwiseColor,
    Processing, Recipe, Stage, VIGNETTE_EFFECT, gpu_plan,
};
use luxforge_reference::{
    preview_error::{self, Class, Rgb8, Statistics},
    srgb,
};
use luxforge_ui::photo_surface::{
    GpuBoundary, GpuPlan, GpuProgram, GpuStep, PositionMap, TexelMap,
    gpu_preview::qualification::{Qualifier, boundary, held},
    validate_step,
};
use serde_json::{Value, json};
use std::sync::Arc;

// ---- Without a device -------------------------------------------------------------------------

/// The core validates its programs against a copy of the surface's prelude; this holds every one
/// to the surface's real prelude and checks, so the two copies cannot drift apart unnoticed.
#[test]
fn gpu_colour_every_shipped_program_passes_the_surfaces_own_convention() {
    assert!(!GPU_PROGRAMS.is_empty());
    let mut entries = std::collections::HashSet::new();
    for shipped in GPU_PROGRAMS {
        assert_eq!(
            shipped.kind,
            GpuProgramKind::Colour,
            "{}: the surface runs colour steps only so far",
            shipped.entry
        );
        assert!(entries.insert(shipped.entry), "{} twice", shipped.entry);
        let step = GpuStep::colour(GpuProgram::new(shipped.entry, shipped.source));
        if let Err(error) = validate_step(&step) {
            panic!("{} does not pass the surface's convention:\n{error}", shipped.entry);
        }
    }
}

fn registry() -> ModuleRegistry {
    ModuleRegistry::builtin()
}

fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        layers,
        ..Recipe::default()
    }
}

/// The full colour stack the corpus check measures: a full Basic layer, a Tone curve, the mixer
/// and a vignette.
fn colour_stack() -> Vec<Layer> {
    vec![
        Layer::new(
            BASIC_EFFECT,
            json!({
                "exposure": 0.5, "contrast": 25.0, "highlights": -30.0, "shadows": 30.0,
                "whites": -15.0, "blacks": 15.0, "vibrance": 30.0, "saturation": 15.0,
                "temperature": 20.0, "tint": -10.0
            }),
        ),
        Layer::new(
            CURVE_EFFECT,
            json!({"luminance": [[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]]}),
        ),
        Layer::new(
            MIXER_EFFECT,
            json!({
                "red-hue": 30, "orange-saturation": -40, "green-saturation": 40,
                "aqua-hue": -25, "blue-luminance": -30, "magenta-saturation": 25
            }),
        ),
        Layer::new(
            VIGNETTE_EFFECT,
            json!({"amount": -60, "midpoint": 40, "roundness": 20, "feather": 60}),
        ),
    ]
}

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

fn planned(registry: &ModuleRegistry, recipe: &Recipe, request: GpuPlanRequest) -> luxforge_core::GpuPlan {
    match gpu_plan(registry, recipe, request).expect("the stack compiles") {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("expected a plan, got {reason}"),
    }
}

fn flat_boundary(width: u32, height: u32) -> GpuBoundary {
    boundary(width, height, 1, &vec![[0.25; 3]; (width * height) as usize]).unwrap()
}

/// A colour plan becomes one colour step per unit, in recipe order, each with its operation's
/// position map and its unit's words and block; what the surface cannot run yet is named.
#[test]
fn gpu_colour_plans_convert_to_one_step_per_unit_or_name_what_the_surface_lacks() {
    let registry = registry();
    let stack = recipe(colour_stack());
    let plan = planned(
        &registry,
        &stack,
        GpuPlanRequest::fit(0, stage(64, 48), stage(640, 480)).qualifying(),
    );
    let converted = surface_plan(&plan, flat_boundary(64, 48)).expect("a runnable plan");
    let entries: Vec<&str> = converted
        .steps
        .iter()
        .map(|step| match step {
            GpuStep::Colour { program, .. } => program.entry.as_ref(),
        })
        .collect();
    assert_eq!(
        entries,
        [
            "lf_basic_white_balance",
            "lf_basic_exposure",
            "lf_basic_tone",
            "lf_basic_colour_adjust",
            "lf_curve_tone_curve",
            "lf_mixer_mixer",
            "lf_vignette_vignette",
        ]
    );
    assert_eq!(converted.texels, TexelMap::IDENTITY);
    for (step, unit) in converted
        .steps
        .iter()
        .zip(plan.content.iter().flat_map(|operation| &operation.units))
    {
        let GpuStep::Colour { program: converted, position } = step;
        assert_eq!(*position, PositionMap::IDENTITY);
        assert_eq!(converted, &program(unit));
        assert_eq!(
            converted.block.len(),
            unit.block.as_ref().map_or(0, |block| block.len())
        );
    }
    // The vignette after a straightened crop runs in output space, after a resample the surface
    // does not draw yet.
    let mut cropped = colour_stack();
    cropped.insert(
        3,
        Layer::new(
            luxforge_core::CROP_EFFECT,
            json!({"angle": 4.0, "x": 0.1, "y": 0.1, "width": 0.8, "height": 0.8}),
        ),
    );
    let plan = planned(
        &registry,
        &recipe(cropped),
        GpuPlanRequest::fit(0, stage(64, 48), stage(640, 480)).qualifying(),
    );
    assert_eq!(plan.output.len(), 1, "the vignette is an output operation");
    assert_eq!(
        surface_plan(&plan, flat_boundary(64, 48)).unwrap_err(),
        Unrunnable::Geometry
    );
    // A boundary that is not the plan's stage.
    let plan = planned(
        &registry,
        &stack,
        GpuPlanRequest::fit(0, stage(64, 48), stage(640, 480)).qualifying(),
    );
    let error = surface_plan(&plan, flat_boundary(32, 48)).unwrap_err();
    assert_eq!(error.code(), "boundary-size");
}

// ---- On a device: the synthetic grid ----------------------------------------------------------

/// A deterministic `splitmix64` stream: the grid is the same on every host and every run.
struct Stream(u64);

impl Stream {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// The values every grid draws its out-of-range extremes from: the largest half floats of both
/// signs, the smallest normal ones, zeros of both signs, and values past white and below black.
const EXTREMES: [f32; 10] = [
    65504.0, -65504.0, 6.104e-5, -6.104e-5, 0.0, -0.0, 2048.0, -16.0, 1.0e-7, 1.0,
];

/// One linear channel: in range (seventy percent, uniform in encoded sRGB so the shadows are as
/// dense as the highlights), past white up to 16, below black down to -0.5, or an extreme.
fn channel(stream: &mut Stream) -> f32 {
    let kind = stream.unit();
    let value = stream.unit();
    if kind < 0.70 {
        srgb::decode_encoded(value) as f32
    } else if kind < 0.82 {
        (1.0 + 15.0 * value * value) as f32
    } else if kind < 0.94 {
        (-0.5 * value) as f32
    } else {
        EXTREMES[(value * EXTREMES.len() as f64) as usize % EXTREMES.len()]
    }
}

/// A dense synthetic grid of `width × height` linear pixels, each channel as the boundary holds it
/// (the nearest half float): one pixel in ten an exact grey, the rest three independent channels.
fn grid(width: u32, height: u32, seed: u64) -> Vec<[f32; 3]> {
    let mut stream = Stream(seed);
    (0..width * height)
        .map(|_| {
            if stream.unit() < 0.1 {
                let grey = held(channel(&mut stream));
                [grey; 3]
            } else {
                [
                    held(channel(&mut stream)),
                    held(channel(&mut stream)),
                    held(channel(&mut stream)),
                ]
            }
        })
        .collect()
}

/// The CPU units `layer` compiles to over `stage`, as the host compiles them.
fn cpu_units(registry: &ModuleRegistry, layer: &Layer, stage: Stage) -> Vec<Arc<dyn PointwiseColor>> {
    let (module, _) = registry.effect(&layer.effect_id).expect("a built-in effect");
    match module
        .compile(
            &layer.effect_id,
            EFFECT_FORMAT,
            &layer.payload,
            CompileStage::exact(stage),
        )
        .expect("the layer compiles")
    {
        Processing::Color(operation) => operation.units().to_vec(),
        other => panic!("expected a colour operation, got {other:?}"),
    }
}

/// What one case measured.
struct Measured {
    /// The program's `f32` output through the reference quantizer, against the CPU unit's output
    /// through the same quantizer: the figures a program is qualified by.
    program: Statistics,
    /// The codes the stage draws, through the hardware's sRGB encoding of the same output, against
    /// the CPU's: the program's figures plus the hardware encoder's rounding, reported beside them.
    drawn: Statistics,
    /// The program meets the pointwise limits and the finiteness rule.
    passed: bool,
    /// Channels the CPU computed finite and the GPU did not.
    non_finite: usize,
    /// Channels the CPU itself computed non-finite, which the hard rule does not cover.
    cpu_non_finite: usize,
    /// The largest difference in linear light, relative to `max(1, |cpu|)`, over the channels the
    /// CPU computed within `[-1, 2]` from a pixel whose input lies within `[-1, 16]`. An extreme
    /// input (a half float's largest value beside small ones) leaves both processors' small
    /// channels as the rounding of an Oklab round trip at that scale, so it says nothing of the
    /// program's precision; its codes still count in the statistics.
    linear: f64,
}

/// `pixels` as 8-bit sRGB, three bytes a pixel, through the independent reference's quantizer.
fn codes(pixels: impl Iterator<Item = [f32; 3]>) -> Vec<u8> {
    pixels
        .flat_map(|rgb| rgb.map(|value| srgb::code(f64::from(value))))
        .collect()
}

/// `layer` over the grid on the device against its CPU units.
fn measure(
    qualifier: &Qualifier,
    registry: &ModuleRegistry,
    layer: &Layer,
    (width, height): (u32, u32),
    seed: u64,
) -> Measured {
    let stage = stage(width, height);
    let inputs = grid(width, height, seed);
    // The CPU: every unit over each row in order, nothing clamped between them.
    let units = cpu_units(registry, layer, stage);
    let mut cpu = inputs.clone();
    for (y, row) in cpu.chunks_mut(width as usize).enumerate() {
        for unit in &units {
            unit.apply_row(y as u32, 0, row);
        }
    }
    // The GPU: the same layer planned from its input, converted and evaluated.
    let plan = planned(
        registry,
        &recipe(vec![layer.clone()]),
        GpuPlanRequest::exact(0, stage).qualifying(),
    );
    let held_boundary = boundary(width, height, seed, &inputs).expect("a boundary");
    let converted = surface_plan(&plan, held_boundary).expect("a runnable plan");
    let gpu = qualifier.evaluate(&converted).expect("a qualification readback");
    let drawn = qualifier
        .evaluate_codes(&converted)
        .expect("a qualification readback");

    let (mut non_finite, mut cpu_non_finite, mut linear) = (0, 0, 0.0_f64);
    for ((cpu, gpu), input) in cpu.iter().zip(&gpu).zip(&inputs) {
        let ordinary = input.iter().all(|value| (-1.0..=16.0).contains(value));
        for channel in 0..3 {
            let (c, g) = (cpu[channel], gpu[channel]);
            if !c.is_finite() {
                cpu_non_finite += 1;
            } else if !g.is_finite() {
                non_finite += 1;
            } else if ordinary && (-1.0..=2.0).contains(&c) {
                let difference = f64::from((g - c).abs()) / f64::from(c.abs().max(1.0));
                linear = linear.max(difference);
            }
        }
    }
    let reference = codes(cpu.iter().copied());
    let candidate: Vec<u8> = drawn
        .iter()
        .flat_map(|code| [code[0], code[1], code[2]])
        .collect();
    let program = codes(gpu.iter().map(|texel| [texel[0], texel[1], texel[2]]));
    let frame = |bytes| Rgb8::new(width, height, bytes).expect("a whole frame");
    let compare = |candidate| {
        preview_error::compare(frame(candidate), frame(&reference), [0, 0, width, height])
            .expect("comparable frames")
    };
    let program = compare(&program);
    let passed = preview_error::verdict(&program, Class::Pointwise).passed() && non_finite == 0;
    Measured {
        program,
        drawn: compare(&candidate),
        passed,
        non_finite,
        cpu_non_finite,
        linear,
    }
}

/// The four statistics and the largest difference, as a report line.
fn figures(s: &Statistics) -> String {
    format!(
        "mean {:.4} worst block {:.4} p99 {:.4} mean dL* {:+.4} max {:.4}",
        s.mean, s.worst_block, s.p99, s.mean_delta_l, s.max
    )
}

/// The worst of each statistic over several cases, the signed ΔL* by its magnitude.
fn worst(statistics: &[Statistics]) -> Statistics {
    let largest = |of: fn(&Statistics) -> f64| {
        statistics.iter().map(of).fold(0.0, f64::max)
    };
    let delta_l = statistics
        .iter()
        .map(|s| s.mean_delta_l)
        .fold(0.0, |a: f64, b| if b.abs() > a.abs() { b } else { a });
    Statistics {
        pixels: statistics.iter().map(|s| s.pixels).sum(),
        mean: largest(|s| s.mean),
        worst_block: largest(|s| s.worst_block),
        worst_block_origin: [0, 0],
        p99: largest(|s| s.p99),
        mean_delta_l: delta_l,
        max: largest(|s| s.max),
    }
}

/// Every case of one unit, measured and reported; each enabled program must meet the pointwise
/// limits and the finiteness rule on every case. A disabled program is reported with its misses.
fn qualify(test: &str, entry: &str, cases: Vec<(String, Layer)>, size: (u32, u32)) {
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let shipped = GPU_PROGRAMS
        .iter()
        .find(|program| program.entry == entry)
        .expect("a shipped program");
    let registry = registry();
    let (mut programs, mut drawn, mut missed) = (Vec::new(), Vec::new(), Vec::new());
    let (mut linear, mut non_finite) = (0.0_f64, 0);
    for (index, (name, layer)) in cases.iter().enumerate() {
        let measured = measure(&qualifier, &registry, layer, size, 0x6c66_0000 + index as u64);
        eprintln!(
            "{entry} {name}: program {} | drawn {} | non-finite {} (CPU non-finite {}) | linear \
             rel {:.2e}{}",
            figures(&measured.program),
            figures(&measured.drawn),
            measured.non_finite,
            measured.cpu_non_finite,
            measured.linear,
            if measured.passed { "" } else { " MISS" }
        );
        programs.push(measured.program);
        drawn.push(measured.drawn);
        linear = linear.max(measured.linear);
        non_finite += measured.non_finite;
        if !measured.passed {
            missed.push(name.clone());
        }
    }
    eprintln!(
        "{entry} worst of {} cases at {}x{}: program {} | drawn {} | non-finite {non_finite} | \
         linear rel {linear:.2e}; enabled {}",
        cases.len(),
        size.0,
        size.1,
        figures(&worst(&programs)),
        figures(&worst(&drawn)),
        shipped.enabled
    );
    if shipped.enabled {
        assert!(missed.is_empty(), "{entry} is enabled and misses on {missed:?}");
    } else if !missed.is_empty() {
        eprintln!("{entry} ships disabled and misses on {missed:?}");
    }
}

fn basic(payload: Value) -> Layer {
    Layer::new(BASIC_EFFECT, payload)
}

/// The size of every grid but the vignette's: 65,536 pixels.
const GRID: (u32, u32) = (256, 256);

#[test]
fn gpu_colour_exposure_meets_the_pointwise_limits() {
    let cases = [-5.0, -1.3, 0.7, 5.0]
        .map(|ev: f64| (format!("{ev:+} EV"), basic(json!({"exposure": ev}))))
        .to_vec();
    qualify(
        "gpu_colour_exposure_meets_the_pointwise_limits",
        "lf_basic_exposure",
        cases,
        GRID,
    );
}

#[test]
fn gpu_colour_white_balance_meets_the_pointwise_limits() {
    let cases = [
        (-100.0, -100.0),
        (-100.0, 100.0),
        (100.0, -100.0),
        (100.0, 100.0),
        (37.0, -12.0),
    ]
    .map(|(temperature, tint): (f64, f64)| {
        (
            format!("temperature {temperature:+} tint {tint:+}"),
            basic(json!({"temperature": temperature, "tint": tint})),
        )
    })
    .to_vec();
    qualify(
        "gpu_colour_white_balance_meets_the_pointwise_limits",
        "lf_basic_white_balance",
        cases,
        GRID,
    );
}

#[test]
fn gpu_colour_tone_meets_the_pointwise_limits() {
    let mut cases = Vec::new();
    for field in ["contrast", "highlights", "shadows", "whites", "blacks"] {
        for value in [-100.0, 100.0] {
            cases.push((format!("{field} {value:+}"), basic(json!({ field: value }))));
        }
    }
    for (name, payload) in [
        (
            "all +100",
            json!({"contrast": 100, "highlights": 100, "shadows": 100, "whites": 100, "blacks": 100}),
        ),
        (
            "all -100",
            json!({"contrast": -100, "highlights": -100, "shadows": -100, "whites": -100, "blacks": -100}),
        ),
        (
            "alternating",
            json!({"contrast": -100, "highlights": 100, "shadows": -100, "whites": 100, "blacks": -100}),
        ),
        (
            "the corpus's Basic",
            json!({"contrast": 25, "highlights": -30, "shadows": 30, "whites": -15, "blacks": 15}),
        ),
    ] {
        cases.push((name.to_owned(), basic(payload)));
    }
    qualify(
        "gpu_colour_tone_meets_the_pointwise_limits",
        "lf_basic_tone",
        cases,
        GRID,
    );
}

#[test]
fn gpu_colour_colour_adjust_meets_the_pointwise_limits() {
    let cases = [
        (-100.0, 0.0),
        (100.0, 0.0),
        (0.0, -100.0),
        (0.0, 100.0),
        (-100.0, -100.0),
        (100.0, 100.0),
        (100.0, -100.0),
        (-100.0, 100.0),
        (30.0, 15.0),
    ]
    .map(|(vibrance, saturation): (f64, f64)| {
        (
            format!("vibrance {vibrance:+} saturation {saturation:+}"),
            basic(json!({"vibrance": vibrance, "saturation": saturation})),
        )
    })
    .to_vec();
    qualify(
        "gpu_colour_colour_adjust_meets_the_pointwise_limits",
        "lf_basic_colour_adjust",
        cases,
        GRID,
    );
}

#[test]
fn gpu_colour_tone_curve_meets_the_pointwise_limits() {
    let zigzag: Vec<[f64; 2]> = (0..16)
        .map(|k| {
            let x = f64::from(k) / 15.0;
            [x, (x + if k % 2 == 0 { 0.0 } else { 0.06 }).min(1.0)]
        })
        .collect();
    let cases = [
        ("the corpus's S-curve", json!([[0.0, 0.0], [0.25, 0.2], [0.75, 0.8], [1.0, 1.0]])),
        ("a steep wall", json!([[0.0, 0.0], [0.5, 0.0], [0.505, 1.0], [1.0, 1.0]])),
        ("a lifted black and lowered white", json!([[0.0, 0.3], [1.0, 0.7]])),
        // Flat at encoded 0.5, which is exactly the threshold between codes 127 and 128, so every
        // pixel lands on it: what separates the program from the hardware encoder.
        ("flat on a code threshold", json!([[0.0, 0.5], [1.0, 0.5]])),
        ("flat", json!([[0.0, 0.45], [1.0, 0.45]])),
        ("crushed ends", json!([[0.2, 0.0], [0.8, 1.0]])),
        ("sixteen knots", json!(zigzag)),
        ("collapsed knots", json!([[0.0, 0.0], [0.4, 0.3], [0.400_000_01, 0.7], [1.0, 1.0]])),
    ]
    .map(|(name, points)| {
        (
            name.to_owned(),
            Layer::new(CURVE_EFFECT, json!({ "luminance": points })),
        )
    })
    .to_vec();
    qualify(
        "gpu_colour_tone_curve_meets_the_pointwise_limits",
        "lf_curve_tone_curve",
        cases,
        GRID,
    );
}

const RANGES: [&str; 8] = [
    "red", "orange", "yellow", "green", "aqua", "blue", "purple", "magenta",
];

#[test]
fn gpu_colour_mixer_meets_the_pointwise_limits() {
    let mut cases = Vec::new();
    for property in ["hue", "saturation", "luminance"] {
        for value in [-100, 100] {
            let payload: serde_json::Map<String, Value> = RANGES
                .iter()
                .map(|range| (format!("{range}-{property}"), json!(value)))
                .collect();
            cases.push((
                format!("every {property} {value:+}"),
                Layer::new(MIXER_EFFECT, Value::Object(payload)),
            ));
        }
    }
    for range in RANGES {
        cases.push((
            format!("{range} at every extreme"),
            Layer::new(
                MIXER_EFFECT,
                json!({
                    format!("{range}-hue"): 100,
                    format!("{range}-saturation"): -100,
                    format!("{range}-luminance"): 100
                }),
            ),
        ));
    }
    let alternating: serde_json::Map<String, Value> = RANGES
        .iter()
        .enumerate()
        .flat_map(|(index, range)| {
            let sign = if index % 2 == 0 { 100 } else { -100 };
            [
                (format!("{range}-hue"), json!(sign)),
                (format!("{range}-saturation"), json!(-sign)),
                (format!("{range}-luminance"), json!(sign)),
            ]
        })
        .collect();
    cases.push((
        "alternating extremes".to_owned(),
        Layer::new(MIXER_EFFECT, Value::Object(alternating)),
    ));
    cases.push((
        "the corpus's mixer".to_owned(),
        Layer::new(
            MIXER_EFFECT,
            json!({
                "red-hue": 30, "orange-saturation": -40, "green-saturation": 40,
                "aqua-hue": -25, "blue-luminance": -30, "magenta-saturation": 25
            }),
        ),
    ));
    qualify(
        "gpu_colour_mixer_meets_the_pointwise_limits",
        "lf_mixer_mixer",
        cases,
        GRID,
    );
}

#[test]
fn gpu_colour_vignette_meets_the_pointwise_limits() {
    let mut cases = Vec::new();
    for amount in [-100, 100] {
        for roundness in [-100, 0, 100] {
            for (midpoint, feather) in [(0, 0), (50, 0), (50, 100), (100, 100), (0, 100)] {
                cases.push((
                    format!(
                        "amount {amount:+} roundness {roundness:+} midpoint {midpoint} feather \
                         {feather}"
                    ),
                    Layer::new(
                        VIGNETTE_EFFECT,
                        json!({
                            "amount": amount, "midpoint": midpoint, "roundness": roundness,
                            "feather": feather
                        }),
                    ),
                ));
            }
        }
    }
    cases.push((
        "the corpus's vignette".to_owned(),
        Layer::new(
            VIGNETTE_EFFECT,
            json!({"amount": -60, "midpoint": 40, "roundness": 20, "feather": 60}),
        ),
    ));
    // A 3:2 stage, so the shape is not a circle.
    qualify(
        "gpu_colour_vignette_meets_the_pointwise_limits",
        "lf_vignette_vignette",
        cases,
        (384, 256),
    );
}

// ---- On a device: the transcendentals --------------------------------------------------------

/// A probe program: the function its second word selects, of the pair of `f32` inputs its block
/// holds for this pixel, the grid's width in its first word. Each case is written as the colour
/// programs write their calls, constant exponents included, so the backend compiles it as it
/// compiles theirs.
const PROBE: &str = "\
fn probe(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let at = block + 2u * (u32(pos.y) * lf_word(words) + u32(pos.x));
    let x = lf_block_f32(at);
    let y = lf_block_f32(at + 1u);
    switch lf_word(words + 1u) {
        case 0u: { return vec3<f32>(pow(x, 1.0f / 2.4f), 0.0, 0.0); }
        case 1u: { return vec3<f32>(pow(x, 2.4), 0.0, 0.0); }
        case 2u: { return vec3<f32>(pow(x, y), 0.0, 0.0); }
        case 3u: { return vec3<f32>(exp(x), exp2(x), 0.0); }
        case 4u: {
            let a = abs(x);
            let estimate = pow(a, 1.0 / 3.0);
            let refined = (2.0 * estimate + a / (estimate * estimate)) / 3.0;
            return vec3<f32>(estimate, refined, 0.0);
        }
        case 5u: { return vec3<f32>(atan2(y, x), 0.0, 0.0); }
        case 6u: { return vec3<f32>(cos(x), sin(x), 0.0); }
        default: { return vec3<f32>(0.0); }
    }
}
";

/// The distance from `value` to `reference` in units of the `f32` spacing at `reference`.
fn ulps(value: f32, reference: f64) -> f64 {
    let narrowed = (reference as f32).abs();
    let spacing = f64::from(narrowed.next_up() - narrowed).max(f64::from(f32::MIN_POSITIVE));
    (f64::from(value) - reference).abs() / spacing
}

/// What one function measured: the largest error in units of the `f32` spacing at the reference
/// and relative to it, the largest absolute error, and how many outputs were not finite.
#[derive(Default)]
struct Precision {
    ulps: f64,
    relative: f64,
    absolute: f64,
    non_finite: usize,
}

impl Precision {
    fn add(&mut self, value: f32, reference: f64) {
        if !value.is_finite() {
            self.non_finite += 1;
            return;
        }
        self.ulps = self.ulps.max(ulps(value, reference));
        let absolute = (f64::from(value) - reference).abs();
        self.absolute = self.absolute.max(absolute);
        if reference != 0.0 {
            self.relative = self.relative.max(absolute / reference.abs());
        }
    }
}

/// Log-uniform in `[low, high]`.
fn log_uniform(stream: &mut Stream, low: f64, high: f64) -> f64 {
    (low.ln() + stream.unit() * (high / low).ln()).exp()
}

/// One probed function: its selector, what it is, its inputs and the `f64` reference of each of
/// its two outputs, `None` for an output it does not have.
struct Probe {
    selector: u32,
    name: &'static str,
    inputs: Vec<[f32; 2]>,
    reference: fn(f64, f64) -> [Option<f64>; 2],
}

/// The functions the colour programs call, over the domains they hand them.
fn probes(count: usize) -> Vec<Probe> {
    let mut stream = Stream(0x7072_6563);
    let mut inputs = |sample: &mut dyn FnMut(&mut Stream, usize) -> [f64; 2]| -> Vec<[f32; 2]> {
        (0..count)
            .map(|index| sample(&mut stream, index).map(|value| value as f32))
            .collect()
    };
    vec![
        Probe {
            selector: 0,
            name: "pow(x, 1/2.4), x in [0.0031308, 65504] (sRGB encode)",
            inputs: inputs(&mut |s, _| [log_uniform(s, 0.003_130_8, 65504.0), 0.0]),
            reference: |x, _| [Some(x.powf(f64::from(1.0_f32 / 2.4_f32))), None],
        },
        Probe {
            selector: 1,
            name: "pow(x, 2.4), x in [0.088, 128] (sRGB decode)",
            inputs: inputs(&mut |s, _| [log_uniform(s, 0.088, 128.0), 0.0]),
            reference: |x, _| [Some(x.powf(f64::from(2.4_f32))), None],
        },
        Probe {
            selector: 2,
            name: "pow(x, y), x in [1e-6, 2], y in [0.125, 8] (mixer L, vignette)",
            inputs: inputs(&mut |s, index| {
                if index % 2 == 0 {
                    [log_uniform(s, 1e-6, 1.0), log_uniform(s, 0.5, 2.0)]
                } else {
                    [log_uniform(s, 1e-4, 2.0), log_uniform(s, 0.125, 8.0)]
                }
            }),
            reference: |x, y| [Some(x.powf(y)), None],
        },
        Probe {
            selector: 3,
            name: "exp(x) (Tone's logistic), exp2(x) (the mixer's gamma), x in [-80, 80]",
            inputs: inputs(&mut |s, index| {
                let x = if index % 4 == 0 {
                    -1.0 + 2.0 * s.unit()
                } else {
                    -80.0 + 160.0 * s.unit()
                };
                [x, 0.0]
            }),
            reference: |x, _| [Some(x.exp()), Some(x.exp2())],
        },
        Probe {
            selector: 4,
            name: "cube root, x in [1e-30, 1e5]: pow(x, 1/3) [0], then one Newton step [1]",
            inputs: inputs(&mut |s, _| [log_uniform(s, 1e-30, 1e5), 0.0]),
            reference: |x, _| [Some(x.cbrt()), Some(x.cbrt())],
        },
        Probe {
            selector: 5,
            name: "atan2(y, x), every quadrant, radius in [1e-6, 100]",
            inputs: inputs(&mut |s, _| {
                let angle = (s.unit() * 2.0 - 1.0) * std::f64::consts::PI;
                let radius = log_uniform(s, 1e-6, 1e2);
                [radius * angle.cos(), radius * angle.sin()]
            }),
            reference: |x, y| [Some(y.atan2(x)), None],
        },
        Probe {
            selector: 6,
            name: "cos(x) [0], sin(x) [1], x in [-pi, pi]",
            inputs: inputs(&mut |s, _| [(s.unit() * 2.0 - 1.0) * std::f64::consts::PI, 0.0]),
            reference: |x, _| [Some(x.cos()), Some(x.sin())],
        },
    ]
}

/// The precision of each transcendental the colour programs use, under this backend's compile
/// options, over the domains the programs hand it: measured, not assumed, against `f64`
/// references of the very `f32` inputs. Every output is finite where the reference is.
#[test]
fn gpu_colour_transcendental_precision_is_measured() {
    let test = "gpu_colour_transcendental_precision_is_measured";
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let (width, height) = (256_u32, 256_u32);
    let mut failures = Vec::new();
    for probe in probes((width * height) as usize) {
        let block: Vec<u32> = probe
            .inputs
            .iter()
            .flat_map(|pair| pair.map(f32::to_bits))
            .collect();
        let plan = GpuPlan {
            boundary: flat_boundary(width, height),
            texels: TexelMap::IDENTITY,
            steps: vec![GpuStep::colour(GpuProgram {
                words: vec![width, probe.selector],
                block: Arc::from(block),
                ..GpuProgram::new("probe", PROBE)
            })],
        };
        let output = qualifier.evaluate(&plan).expect("a probe readback");
        let mut channels = [Precision::default(), Precision::default()];
        for (pair, texel) in probe.inputs.iter().zip(&output) {
            let expected = (probe.reference)(f64::from(pair[0]), f64::from(pair[1]));
            for (channel, expected) in expected.iter().enumerate() {
                if let Some(expected) = expected {
                    channels[channel].add(texel[channel], *expected);
                }
            }
        }
        for (channel, precision) in channels.iter().enumerate() {
            if (probe.reference)(1.0, 1.0)[channel].is_none() {
                continue;
            }
            eprintln!(
                "{} [output {channel}]: max {:.1} ulp, relative {:.2e}, absolute {:.2e}, \
                 non-finite {}",
                probe.name,
                precision.ulps,
                precision.relative,
                precision.absolute,
                precision.non_finite
            );
            if precision.non_finite > 0 {
                failures.push(format!("{} [output {channel}]", probe.name));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "non-finite where the reference is finite: {failures:?}"
    );
}

// ---- The corpus at Fit ------------------------------------------------------------------------

/// The corpus's colour families, which the colour programs are qualified on at Fit.
const COLOUR_FAMILIES: [&str; 5] = ["basic", "tone-curve", "mixer", "vignette", "colour-stack"];

/// The Fit bounds of an evidence run's window, 1440 × 900 logical at 2× with both panels open,
/// as the desktop computes them ([`crate::app::Editor::proxy_bounds`]).
fn fit_bounds() -> luxforge_core::ProxyBounds {
    let surface = crate::layout::photo_surface((1440.0, 900.0), true, true);
    let inset = crate::layout::FIT_INSET;
    crate::app::preview::bounds_of(((surface.0 - inset.0) * 2.0, (surface.1 - inset.1) * 2.0))
        .expect("room for a photograph")
}

/// One corpus cell's photograph: its file and the corpus's id for it.
struct CorpusSource {
    id: String,
    path: std::path::PathBuf,
    raw: bool,
}

/// The corpus's sources this host has: the generated JPEGs under `generated`, and the RAWs the
/// private manifest at `manifest` resolves, when it is given. A source without a file here is
/// named and skipped.
fn corpus_sources(
    corpus: &Value,
    generated: &std::path::Path,
    manifest: Option<&Value>,
) -> Vec<CorpusSource> {
    let mut found = Vec::new();
    for source in corpus["sources"].as_array().expect("sources") {
        let id = source["id"].as_str().expect("an id").to_owned();
        let path = match source["kind"].as_str() {
            Some("generated-jpeg") => source["path"]
                .as_str()
                .and_then(|path| std::path::Path::new(path).file_name())
                .map(|name| generated.join(name)),
            Some("raw") => manifest.and_then(|manifest| {
                manifest["sources"]
                    .as_array()?
                    .iter()
                    .find(|entry| entry["id"] == source["manifest_id"])
                    .and_then(|entry| entry["path"].as_str())
                    .map(std::path::PathBuf::from)
            }),
            _ => None,
        };
        match path.filter(|path| path.is_file()) {
            Some(path) => found.push(CorpusSource {
                raw: source["kind"] == "raw",
                id,
                path,
            }),
            None => eprintln!("gap: {id} has no file on this host"),
        }
    }
    found
}

/// What one cell measured, or why it has no figures.
enum Cell {
    Measured {
        /// The stage the Fit frame was rendered at, and whether it was a proxy.
        stage: (u32, u32),
        proxy: bool,
        /// The codes the stage draws against the CPU's: the figures the limits judge.
        statistics: Statistics,
        /// The GPU's `f32` output through the reference quantizer against the CPU's, which leaves
        /// the hardware encoder's rounding out.
        program: Statistics,
        passed: bool,
    },
    Gap(String),
}

/// The stack's current recipe after `steps`, on a fresh catalog holding `source` alone.
fn apply_steps(
    owner: &luxforge_core::OwnerHandle,
    client: luxforge_core::ClientId,
    asset: &luxforge_core::AssetId,
    steps: &[Value],
) -> Result<(), String> {
    use luxforge_testkit::client::{call, mutation, request_id, revision};
    let id = json!(asset.as_str());
    for step in steps {
        let Some(api) = step.get("api") else {
            return Err(format!("{step} is not an API step"));
        };
        let mut params = api["params"].clone();
        params["asset_id"] = id.clone();
        params["mutation"] = mutation(revision(owner, client, &id)?, &request_id("corpus"), "agent");
        call(owner, client, api["method"].as_str().expect("a method"), params)?;
    }
    Ok(())
}

/// The CPU proxy frame and the GPU frame of one recipe on one source at Fit, both written as PNGs
/// in `output` under `name`, and their figures.
fn corpus_cell(
    qualifier: &Qualifier,
    source: &CorpusSource,
    steps: &[Value],
    output: &std::path::Path,
    name: &str,
) -> Result<Cell, String> {
    use luxforge_core::{
        Cancel, EffectStage, OwnerHandle, PhaseOutcome, PreviewIntent, PreviewQueue,
        PreviewRequest, PreviewSource, RenderContext, RenderOptions, render,
    };
    let catalog = output.join(format!("{name}.sqlite"));
    let (owner, join) = OwnerHandle::start(&catalog).map_err(|error| error.to_string())?;
    let client = owner.register();
    let finish = |owner: OwnerHandle, join: std::thread::JoinHandle<()>| {
        owner.stop();
        let _ = join.join();
    };
    let asset = crate::app::testing::import_and_adopt(&owner, client, &source.path);
    let result = (|| -> Result<Cell, String> {
        apply_steps(&owner, client, &asset, steps)?;
        let recipe =
            luxforge_testkit::client::recipe(&owner, client, &json!(asset.as_str()))?;
        let bounds = fit_bounds();
        // The CPU frame: the desktop's own Fit job, through the preview worker's proxy phase.
        let mut job = crate::app::tasks::ready_preview_job(
            &owner,
            PreviewRequest::new(client, asset.clone()).proxy(bounds),
        )?;
        job.intent = PreviewIntent::Interactive;
        let evaluation = job.evaluation.clone();
        let mut queue = PreviewQueue::default();
        let generation = queue.request(job);
        let outcome = luxforge_testbase::wait_for("the Fit frame", || {
            let result = queue.poll()?;
            (result.generation == generation).then_some(result.outcome)
        });
        let registry = evaluation.registry().clone();
        let full = evaluation.source().dimensions();
        // The Fit frame and the source it was rendered from: the proxy phase's frame over the
        // proxy the worker built, or, for a photograph that fits the bounds at its own size, the
        // exact phase's frame over the source itself.
        let (cpu, proxied, is_proxy) = match outcome {
            PhaseOutcome::Proxy(proxy) => {
                let context = RenderContext::new();
                let plan = render(
                    &registry,
                    evaluation.source(),
                    evaluation.recipe(),
                    RenderOptions::exact(&Cancel::never()),
                    &context,
                )
                .map_err(|error| error.to_string())?
                .proxy_plan(bounds)
                .ok_or("a proxy frame without a proxy plan")?;
                let proxied = evaluation
                    .source()
                    .proxy(plan)
                    .map_err(|error| error.to_string())?;
                if proxied.dimensions() != proxy.dimensions {
                    return Ok(Cell::Gap(format!(
                        "the worker's proxy is {:?}, not this {:?}",
                        proxy.dimensions,
                        proxied.dimensions()
                    )));
                }
                (proxy.raster, proxied, true)
            }
            PhaseOutcome::Exact(exact) => (
                exact.result.map_err(|error| error.to_string())?,
                evaluation.source().clone(),
                false,
            ),
            PhaseOutcome::Region(_) => return Ok(Cell::Gap("a region at Fit".into())),
        };
        let (width, height) = proxied.dimensions();
        if (cpu.width, cpu.height) != (width, height) {
            return Ok(Cell::Gap(format!(
                "the frame is {}x{}, not its source's {width}x{height}",
                cpu.width, cpu.height
            )));
        }
        // The boundary: the input of the first colour layer, which is the source the frame was
        // rendered from when every layer before it compiles to the identity there, as a RAW
        // development and a reset lens profile do.
        let stage_of = |layer: &Layer| registry.effect(&layer.effect_id).map(|(_, e)| e.stage);
        let boundary_layer = recipe
            .layers
            .iter()
            .position(|layer| {
                matches!(
                    stage_of(layer),
                    Some(EffectStage::Color | EffectStage::Finish)
                )
            })
            .ok_or("no colour layer")?;
        for layer in &recipe.layers[..boundary_layer] {
            let (module, _) = registry.effect(&layer.effect_id).ok_or("an unknown effect")?;
            let compiled = module
                .compile(
                    &layer.effect_id,
                    layer.effect_format,
                    &layer.payload,
                    CompileStage::exact(stage(width, height)),
                )
                .map_err(|error| error.to_string())?;
            let identity = matches!(
                compiled,
                Processing::ExactGeometry(g)
                    if (g.a, g.b, g.c, g.d, g.tx, g.ty) == (1, 0, 0, 1, 0, 0)
                        && (g.output_width, g.output_height) == (width, height)
            );
            if !identity {
                return Ok(Cell::Gap(format!(
                    "{} before the colour layers is not the identity",
                    layer.effect_id
                )));
            }
        }
        let texels: Vec<[f32; 3]> = match &proxied {
            PreviewSource::Jpeg(image) => {
                let table = luxforge_core::colour::srgb::decode_table();
                image
                    .rgba
                    .chunks_exact(4)
                    .map(|pixel| [0, 1, 2].map(|c| table[usize::from(pixel[c])]))
                    .collect()
            }
            PreviewSource::Raw { image, settings } => {
                if settings.white_balance.is_some() {
                    return Ok(Cell::Gap("an approximated white balance".into()));
                }
                (0..height)
                    .flat_map(|y| (0..width).map(move |x| (x, y)))
                    .map(|(x, y)| image.pixel(x, y).expect("a viewed pixel"))
                    .collect()
            }
        };
        let request = if is_proxy {
            GpuPlanRequest::fit(boundary_layer, stage(width, height), stage(full.0, full.1))
        } else {
            GpuPlanRequest::exact(boundary_layer, stage(width, height))
        }
        .qualifying();
        let plan = match gpu_plan(&registry, &recipe, request).map_err(|e| e.to_string())? {
            GpuAnswer::Plan(plan) => *plan,
            GpuAnswer::Fallback(reason) => {
                return Ok(Cell::Gap(format!("{}: {reason}", reason.code())));
            }
        };
        let held = boundary(width, height, 1, &texels).ok_or("a boundary")?;
        let converted = match surface_plan(&plan, held) {
            Ok(converted) => converted,
            Err(reason) => {
                return Ok(Cell::Gap(format!(
                    "{}: the surface cannot run {reason:?} yet",
                    reason.code()
                )));
            }
        };
        let drawn = qualifier.evaluate_codes(&converted)?;
        let gpu: Vec<u8> = drawn
            .iter()
            .flat_map(|code| [code[0], code[1], code[2]])
            .collect();
        let program = codes(
            qualifier
                .evaluate(&converted)?
                .iter()
                .map(|texel| [texel[0], texel[1], texel[2]]),
        );
        let reference: Vec<u8> = cpu
            .rgba
            .chunks_exact(4)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
            .collect();
        for (suffix, bytes) in [("gpu", &gpu), ("cpu", &reference)] {
            image::RgbImage::from_raw(width, height, bytes.clone())
                .ok_or("a whole frame")?
                .save(output.join(format!("{name}-{suffix}.png")))
                .map_err(|error| error.to_string())?;
        }
        let frame = |bytes| Rgb8::new(width, height, bytes);
        let statistics =
            preview_error::compare(frame(&gpu)?, frame(&reference)?, [0, 0, width, height])?;
        let program =
            preview_error::compare(frame(&program)?, frame(&reference)?, [0, 0, width, height])?;
        Ok(Cell::Measured {
            stage: (width, height),
            proxy: is_proxy,
            passed: preview_error::verdict(&statistics, Class::Pointwise).passed(),
            statistics,
            program,
        })
    })();
    finish(owner, join);
    let _ = std::fs::remove_file(&catalog);
    result
}

/// The qualification corpus's colour recipes at Fit: for each source this host has, the CPU proxy
/// frame the preview worker renders and the GPU frame of the same plan over the same proxy source,
/// written as `<recipe>--<source>-{cpu,gpu}.png` with the commands that run
/// `cargo xtask preview-error --class pointwise` over each pair. A RAW photograph's first open
/// commits its lens profile, a geometry tail the photo surface does not draw yet; each RAW cell is
/// measured again with that profile reset, which the cell's name says.
///
/// ```sh
/// LUXFORGE_GPU_CORPUS_OUTPUT=/tmp/new-dir \
/// LUXFORGE_GENERATED_FIXTURES=fixtures/generated \
/// LUXFORGE_RAW_MANIFEST=/path/to/raw-manifest.json \
/// cargo test --release -p luxforge-app gpu_colour_corpus -- --ignored --nocapture
/// ```
#[test]
#[ignore = "the GPU preview corpus at Fit: set LUXFORGE_GPU_CORPUS_OUTPUT to a new directory, \
            LUXFORGE_GENERATED_FIXTURES to the generated JPEGs and, for the RAWs, \
            LUXFORGE_RAW_MANIFEST"]
fn gpu_colour_corpus_at_fit() {
    let test = "gpu_colour_corpus_at_fit";
    let output = std::path::PathBuf::from(
        std::env::var("LUXFORGE_GPU_CORPUS_OUTPUT").expect("LUXFORGE_GPU_CORPUS_OUTPUT"),
    );
    assert!(!output.exists(), "{} exists: use a new directory", output.display());
    let Some(qualifier) = Qualifier::headless(test) else {
        return;
    };
    std::fs::create_dir_all(&output).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &std::path::Path| -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    };
    let corpus = read(&root.join("fixtures/preview/corpus.json"));
    let generated = std::env::var("LUXFORGE_GENERATED_FIXTURES")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| root.join("fixtures/generated"));
    let manifest = std::env::var("LUXFORGE_RAW_MANIFEST")
        .ok()
        .map(|path| read(std::path::Path::new(&path)));
    let sources = corpus_sources(&corpus, &generated, manifest.as_ref());
    eprintln!(
        "{test}: adapter {}, Fit bounds {:?}",
        qualifier.adapter(),
        fit_bounds()
    );
    let reset_lens = json!({"api": {"method": "edit.reset-lens-profile", "params": {}}});
    let (mut cells, mut commands, mut missed) = (Vec::new(), Vec::new(), Vec::new());
    for recipe in corpus["recipes"].as_array().expect("recipes") {
        let family = recipe["family"].as_str().unwrap_or_default();
        if !COLOUR_FAMILIES.contains(&family) {
            continue;
        }
        let steps = recipe["steps"].as_array().expect("steps").clone();
        for source in &sources {
            if !recipe["sources"]
                .as_array()
                .expect("sources")
                .iter()
                .any(|id| id == source.id.as_str())
            {
                continue;
            }
            let mut variants = vec![(String::new(), steps.clone())];
            if source.raw {
                let mut reset = vec![reset_lens.clone()];
                reset.extend(steps.iter().cloned());
                variants.push(("--lens-reset".to_owned(), reset));
            }
            for (suffix, steps) in variants {
                let name = format!("{}--{}{suffix}", recipe["id"].as_str().unwrap(), source.id);
                let cell = corpus_cell(&qualifier, source, &steps, &output, &name)
                    .unwrap_or_else(|error| Cell::Gap(format!("failed: {error}")));
                match &cell {
                    Cell::Measured {
                        stage: proxy,
                        proxy: is_proxy,
                        statistics,
                        program,
                        passed,
                    } => {
                        eprintln!(
                            "{name} at {}x{}{}: drawn {} | program {}{}",
                            proxy.0,
                            proxy.1,
                            if *is_proxy { "" } else { " (exact)" },
                            figures(statistics),
                            figures(program),
                            if *passed { "" } else { " MISS" }
                        );
                        commands.push(format!(
                            "cargo xtask preview-error --candidate {dir}/{name}-gpu.png \
                             --reference {dir}/{name}-cpu.png --photo-rect 0,0,{w},{h} \
                             --class pointwise --output {dir}/{name}.json",
                            dir = output.display(),
                            w = proxy.0,
                            h = proxy.1
                        ));
                        if !passed {
                            missed.push(name.clone());
                        }
                        let stats = |s: &Statistics| {
                            json!({
                                "mean": s.mean, "worst_block": s.worst_block, "p99": s.p99,
                                "mean_delta_l": s.mean_delta_l, "max": s.max
                            })
                        };
                        cells.push(json!({
                            "cell": name, "stage": [proxy.0, proxy.1], "proxy": is_proxy,
                            "drawn": stats(statistics), "program": stats(program),
                            "passed": passed
                        }));
                    }
                    Cell::Gap(reason) => {
                        eprintln!("{name}: gap: {reason}");
                        cells.push(json!({"cell": name, "gap": reason}));
                    }
                }
            }
        }
    }
    std::fs::write(
        output.join("cells.json"),
        serde_json::to_string_pretty(&json!({
            "adapter": qualifier.adapter(),
            "bounds": [fit_bounds().width, fit_bounds().height],
            "cells": cells
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(output.join("commands.sh"), commands.join("\n") + "\n").unwrap();
    eprintln!("{test}: {} cells in {}", cells.len(), output.display());
    assert!(missed.is_empty(), "cells missing the pointwise limits: {missed:?}");
}
