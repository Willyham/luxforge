//! The GPU coverage programs of every mask kind (`docs/design/gpu-preview.md`), qualified against
//! their CPU fields on this host's device, and the conversion that hands a masked plan to the photo
//! surface.
//!
//! - Every coverage program the core ships passes the surface's own calling convention inside a
//!   masked step.
//! - A masked plan converts to one masked step: the operation's units and its mask's coverage, with
//!   the plan's modes, inversions, amount, bounds, position and supersample.
//! - Each kind's coverage, composed by the surface's own masked step and read back as `f32`, is held
//!   to the CPU's coverage of the same mask over the same half-float inputs: no non-finite value,
//!   and its half-coverage contour within a quarter of a stage pixel of the CPU's.
//! - A stroke painted over 200 ticks keeps its storage block within the stroke limits, only appends
//!   its new segments, and draws frames within the pointwise limits at every sampled tick.
//!
//! A test with no adapter prints that it was skipped and asserts nothing: the skip is the report,
//! and `cargo test` counting it as passed does not make it GPU evidence.
use super::gpu_plan::{coverage, surface_plan};
use super::gpu_qualification::{corpus_at_fit, figures, grid, held_to_whole, worst};
use luxforge_core::{
    BASIC_EFFECT, Cancel, Component, ComponentMode, GPU_PROGRAMS, GpuAnswer, GpuPlanRequest,
    GpuProgramKind, Layer, MASK_GPU_PROGRAMS, Mask, ModuleRegistry, Recipe, RenderContext,
    RenderOptions, SnapshotId, SourceImage, Stage, gpu_plan,
    mask::{BRUSH_GPU_BLOCK_WORDS_MAX, ColourLimit, CompiledMask, Stroke},
    path::StrokeTable,
    render,
};
use luxforge_reference::preview_error::{self, Class, Rgb8, Statistics};
use luxforge_ui::photo_surface::{
    Coverage, CoverageComponent, CoverageMode, GpuPlan, GpuProgram, GpuStep, MaskedColour,
    PositionMap, TexelMap,
    gpu_preview::qualification::{Qualifier, boundary, held},
    validate_step,
};
use serde_json::{Value, json};
use std::sync::Arc;

fn stage(width: u32, height: u32) -> Stage {
    Stage { width, height }
}

/// The unit every coverage readback runs: one added to every channel, so a masked step's output
/// less its input is its coverage, `(1 − M)·in + M·(in + 1) = in + M`.
fn add_one() -> GpuProgram {
    GpuProgram::new(
        "lf_test_add_one",
        "fn lf_test_add_one(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) \
         -> vec3<f32> {\n    return rgb + vec3<f32>(1.0);\n}\n",
    )
}

/// The masked Basic layer every case plans its mask through, the corpus's own.
fn basic() -> Layer {
    Layer::new(
        BASIC_EFFECT,
        json!({"exposure": 0.8, "contrast": 20.0, "vibrance": 30.0}),
    )
}

fn masked_recipe(mask: &Mask, strokes: &StrokeTable) -> Recipe {
    Recipe {
        layers: vec![Layer {
            mask: Some(mask.id.clone()),
            ..basic()
        }],
        masks: vec![mask.clone()],
        strokes: strokes.clone(),
        ..Recipe::default()
    }
}

fn planned(recipe: &Recipe, request: GpuPlanRequest) -> luxforge_core::GpuPlan {
    match gpu_plan(&ModuleRegistry::builtin(), recipe, request).expect("the stack compiles") {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("expected a plan, got {reason}"),
    }
}

/// One mask of `components`, each `(kind, mode, invert, payload)`.
fn mask_of(components: &[(&str, ComponentMode, bool, Value)], invert: bool, amount: f64) -> Mask {
    let mut mask = Mask::new("Mask 1");
    mask.invert = invert;
    mask.amount = amount;
    for (kind, mode, inverted, payload) in components {
        let name = mask.next_component_name(kind);
        let mut component = Component::new(name, *mode, *kind, payload.clone());
        component.invert = *inverted;
        mask.components.push(component);
    }
    mask
}

// ---- Without a device -------------------------------------------------------------------------

/// Every coverage program the core ships, inside a masked step, passes the surface's own prelude
/// and checks with the coverage signature; no two shipped programs share an entry.
#[test]
fn gpu_mask_every_coverage_program_passes_the_surfaces_own_convention() {
    assert!(!MASK_GPU_PROGRAMS.is_empty());
    let mut entries = std::collections::HashSet::new();
    for shipped in GPU_PROGRAMS.iter().chain(MASK_GPU_PROGRAMS) {
        assert!(entries.insert(shipped.entry), "{} twice", shipped.entry);
    }
    for shipped in MASK_GPU_PROGRAMS {
        assert_eq!(shipped.kind, GpuProgramKind::Coverage, "{}", shipped.entry);
        let step = GpuStep::Masked(MaskedColour {
            units: vec![add_one()],
            position: PositionMap::IDENTITY,
            mask: Coverage {
                position: PositionMap::IDENTITY,
                bounds: [0, 0, 1, 1],
                supersample: false,
                components: vec![CoverageComponent {
                    mode: CoverageMode::Add,
                    invert: false,
                    program: GpuProgram::new(shipped.entry, shipped.source),
                }],
                invert: false,
                scale: 1.0,
            },
        });
        if let Err(error) = validate_step(&step) {
            panic!(
                "{} does not pass the surface's convention:\n{error}",
                shipped.entry
            );
        }
    }
}

/// A masked operation converts to one masked step: its units as colour programs at the operation's
/// position, and its mask's components with their programs, modes and inversions, the mask's
/// inversion and amount, its position, its bounds as a half-open rectangle and the supersample.
#[test]
fn gpu_mask_a_masked_plan_converts_to_one_masked_step() {
    let mask = mask_of(
        &[
            (
                "linear",
                ComponentMode::Add,
                false,
                json!({"x0": 0.5, "y0": 0.2, "x1": 0.5, "y1": 0.8}),
            ),
            (
                "radial",
                ComponentMode::Subtract,
                true,
                json!({"x": 0.5, "y": 0.5, "radius_x": 0.28, "radius_y": 0.22, "angle": 18.0,
                       "feather": 45.0}),
            ),
            (
                "luminance-range",
                ComponentMode::Intersect,
                false,
                json!({"low": 20.0, "low_feather": 10.0, "high": 80.0, "high_feather": 10.0}),
            ),
        ],
        true,
        65.0,
    );
    let recipe = masked_recipe(&mask, &StrokeTable::default());
    let plan = planned(
        &recipe,
        GpuPlanRequest::exact(0, stage(320, 200)).qualifying(),
    );
    let operation = &plan.content[0];
    let core_mask = operation.mask.as_ref().expect("a masked operation");
    let held_boundary = boundary(320, 200, 1, &vec![[0.25; 3]; 320 * 200]).unwrap();
    let converted = surface_plan(&plan, held_boundary).expect("a runnable plan");
    assert_eq!(converted.texels, TexelMap::IDENTITY);
    let [GpuStep::Masked(step)] = &converted.steps[..] else {
        panic!("one masked step, not {:?}", converted.steps.len());
    };
    let units: Vec<&str> = step.units.iter().map(|unit| unit.entry.as_ref()).collect();
    let expected: Vec<&str> = operation
        .units
        .iter()
        .map(|unit| unit.program.entry)
        .collect();
    assert_eq!(units, expected);
    assert_eq!(step.position, PositionMap::IDENTITY);
    let mask = &step.mask;
    assert_eq!(mask.position, PositionMap::IDENTITY);
    assert_eq!(
        mask.bounds,
        [0, 0, 320, 200],
        "an inverted mask covers the stage"
    );
    assert!(mask.invert && !mask.supersample);
    assert_eq!(mask.scale, 0.65);
    let shape: Vec<(CoverageMode, bool, &str)> = mask
        .components
        .iter()
        .map(|component| {
            (
                component.mode,
                component.invert,
                component.program.entry.as_ref(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            (CoverageMode::Add, false, "lf_mask_linear"),
            (CoverageMode::Subtract, true, "lf_mask_radial"),
            (CoverageMode::Intersect, false, "lf_mask_luminance_range"),
        ]
    );
    for (component, core) in mask.components.iter().zip(&core_mask.components) {
        assert_eq!(component.program.words, core.program.words);
    }
    // The converted coverage alone, as the readback tests use it.
    assert_eq!(coverage(core_mask).as_ref(), Some(mask));
}

// ---- The half-coverage contour ----------------------------------------------------------------

/// How far the GPU's half-coverage contour lies from the CPU's, in stage pixels: along every edge
/// between two neighbouring pixel centres that either contour crosses, the distance between the two
/// crossings, each where its field's linear interpolation along that edge reaches one half. Where
/// one field does not cross on that edge, its crossing is extrapolated along the same line, which is
/// how far its contour would have to move to cross there; an edge one field crosses while the other
/// is flat along it is infinitely far. So a pixel that one coverage puts on the other side of one
/// half counts by how far that moves the contour, which is small where the field is steep there and
/// large where it is flat, not by the pixel. An edge touching a pixel in `ties` is left to the
/// tie's own figure ([`tie`]).
fn contour_displacement(
    cpu: &[f64],
    gpu: &[f64],
    ties: &[bool],
    width: u32,
    height: u32,
) -> (f64, usize) {
    let crossing = |a: f64, b: f64| (0.5 - a) / (b - a);
    let crosses = |a: f64, b: f64| (a < 0.5) != (b < 0.5);
    let (mut largest, mut crossings) = (0.0f64, 0);
    let mut edge = |p: usize, q: usize| {
        let (a, b, ga, gb) = (cpu[p], cpu[q], gpu[p], gpu[q]);
        if !crosses(a, b) && !crosses(ga, gb) {
            return;
        }
        crossings += usize::from(crosses(a, b));
        if ties[p] || ties[q] {
            return;
        }
        let moved = (crossing(a, b) - crossing(ga, gb)).abs();
        largest = largest.max(if moved.is_nan() { f64::INFINITY } else { moved });
    };
    for y in 0..height {
        for x in 0..width {
            let p = (y * width + x) as usize;
            if x + 1 < width {
                edge(p, p + 1);
            }
            if y + 1 < height {
                edge(p, p + width as usize);
            }
        }
    }
    (largest, crossings)
}

/// How many times finer than the stage a tie is looked for: the CPU's field is read at its samples
/// moved by a step of this stage. Point samples sit on its pixel centres for an odd factor, and the
/// supersample's quarter positions for a factor of `4n + 2`.
const POINT_FINE: u32 = 15;
const SUPERSAMPLED_FINE: u32 = 30;

/// Whether the GPU's coverage of one pixel that disagrees with the CPU's is a tie: the CPU's own
/// coverage of that pixel, with its samples moved together by at most one step of a stage
/// [`POINT_FINE`] or [`SUPERSAMPLED_FINE`] times finer, or its input scaled by `1 ± 1e-5`, takes the
/// GPU's value. That is a hard edge, or a value threshold, passing within a fifteenth of a pixel of a
/// sample, where `f32` and `f64` may each take either side; the distance it moved, in stage pixels,
/// when it is one.
fn tie(
    fine: &CompiledMask,
    supersample: bool,
    (x, y): (u32, u32),
    rgb: [f64; 3],
    gpu: f64,
) -> Option<f64> {
    let factor = if supersample {
        SUPERSAMPLED_FINE
    } else {
        POINT_FINE
    };
    let mut best: Option<f64> = None;
    for scale in [1.0, 1.0 - 1e-5, 1.0 + 1e-5] {
        let rgb = rgb.map(|channel| channel * scale);
        for dy in -1i64..=1 {
            for dx in -1i64..=1 {
                let at = |base: u32, offset: i64, delta: i64| {
                    (i64::from(base) * i64::from(factor) + offset + delta) as u32
                };
                let value = if supersample {
                    [(7, 7), (22, 7), (7, 22), (22, 22)]
                        .map(|(i, j)| fine.coverage(at(x, i, dx), at(y, j, dy), rgb))
                        .iter()
                        .sum::<f64>()
                        * 0.25
                } else {
                    let centre = i64::from(factor / 2);
                    fine.coverage(at(x, centre, dx), at(y, centre, dy), rgb)
                };
                if (value - gpu).abs() <= 1e-4 {
                    let moved = ((dx * dx + dy * dy) as f64).sqrt() / f64::from(factor);
                    best = Some(best.map_or(moved, |held| held.min(moved)));
                }
            }
        }
    }
    best
}

// ---- The coverage readback --------------------------------------------------------------------

/// Where a case is planned: a Fit proxy of a full stage, whose masks take the thin-feature rule,
/// or the exact phase over its own stage.
#[derive(Clone, Copy)]
enum Phase {
    Fit { proxy: Stage, full: Stage },
    Exact(Stage),
}

impl Phase {
    fn stage(self) -> Stage {
        match self {
            Self::Fit { proxy, .. } => proxy,
            Self::Exact(stage) => stage,
        }
    }

    fn request(self) -> GpuPlanRequest {
        match self {
            Self::Fit { proxy, full } => GpuPlanRequest::fit(0, proxy, full),
            Self::Exact(stage) => GpuPlanRequest::exact(0, stage),
        }
        .qualifying()
    }

    fn name(self) -> String {
        match self {
            Self::Fit { proxy, .. } => format!("Fit {}x{}", proxy.width, proxy.height),
            Self::Exact(stage) => format!("exact {}x{}", stage.width, stage.height),
        }
    }
}

/// What one case measured.
struct Measured {
    supersample: bool,
    largest: f64,
    mean: f64,
    contour: f64,
    crossings: usize,
    /// Pixels whose coverage disagrees by more than a thousandth at a tie, and the farthest a tie
    /// moved the CPU's samples to take the GPU's value.
    ties: usize,
    tie_moved: f64,
}

/// A photograph-like input with structure everywhere: a hue wheel around the frame's centre, its
/// saturation growing outwards, over a lightness falling down the frame, as 8-bit sRGB codes
/// decoded as a JPEG's are, and held as the boundary holds them.
fn photograph(width: u32, height: u32) -> (Vec<u8>, Vec<[f32; 3]>) {
    let mut codes = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            let (u, v) = (
                f64::from(x) / f64::from(width - 1),
                f64::from(y) / f64::from(height - 1),
            );
            let (dx, dy) = (u - 0.5, (v - 0.5) * f64::from(height) / f64::from(width));
            let hue = (dy.atan2(dx) / std::f64::consts::TAU + 0.5) * 6.0;
            let saturation = (dx.hypot(dy) * 2.4).min(1.0);
            let value = 0.08 + 0.9 * (1.0 - v) * (0.75 + 0.25 * (u * 7.0).sin());
            let sector = hue.floor();
            let f = hue - sector;
            let (p, q, t) = (
                value * (1.0 - saturation),
                value * (1.0 - saturation * f),
                value * (1.0 - saturation * (1.0 - f)),
            );
            let rgb = match sector as i64 % 6 {
                0 => [value, t, p],
                1 => [q, value, p],
                2 => [p, value, t],
                3 => [p, q, value],
                4 => [t, p, value],
                _ => [value, p, q],
            };
            codes.extend(rgb.map(|channel| (channel.clamp(0.0, 1.0) * 255.0).round() as u8));
            codes.push(255);
        }
    }
    let table = luxforge_core::colour::srgb::decode_table();
    let pixels = codes
        .chunks_exact(4)
        .map(|pixel| [0, 1, 2].map(|channel| held(table[usize::from(pixel[channel])])))
        .collect();
    (codes, pixels)
}

/// `mask` planned through a masked Basic layer at `phase`, its coverage composed by the surface's
/// masked step over `pixels` and read back, against the CPU's coverage of the same mask over the
/// same values: the point-sampled field, or the mean of the doubled stage's four pixels when the
/// plan took the thin-feature rule.
fn measure(
    qualifier: &Qualifier,
    mask: &Mask,
    strokes: &StrokeTable,
    phase: Phase,
    pixels: &[[f32; 3]],
) -> Measured {
    let Stage { width, height } = phase.stage();
    let plan = planned(&masked_recipe(mask, strokes), phase.request());
    let core = plan.content[0].mask.as_ref().expect("a masked operation");
    let converted = coverage(core).expect("a runnable mask");
    let readback = GpuPlan {
        boundary: boundary(width, height, 1, pixels).expect("a boundary"),
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::Masked(MaskedColour {
            units: vec![add_one()],
            position: PositionMap::IDENTITY,
            mask: converted,
        })],
        region: None,
        lights: Vec::new(),
    };
    let drawn = qualifier.evaluate(&readback).expect("a qualification pass");
    let gpu: Vec<f64> = drawn
        .iter()
        .zip(pixels)
        .map(|(out, input)| f64::from(out[1]) - f64::from(input[1]))
        .collect();
    let compiled = CompiledMask::new(mask, phase.stage(), strokes).expect("a mask");
    if let Phase::Fit { proxy, .. } = phase {
        assert_eq!(
            core.supersample,
            compiled.min_feature_px(proxy) < 2.0,
            "the plan takes the thin-feature rule exactly when the CPU's proxy does"
        );
    }
    let fine = core.supersample.then(|| {
        CompiledMask::new(mask, stage(2 * width, 2 * height), strokes).expect("a doubled mask")
    });
    let cpu: Vec<f64> = (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let rgb = pixels[index as usize].map(f64::from);
            match &fine {
                None => compiled.coverage(x, y, rgb),
                Some(fine) => {
                    (fine.coverage(2 * x, 2 * y, rgb)
                        + fine.coverage(2 * x + 1, 2 * y, rgb)
                        + fine.coverage(2 * x, 2 * y + 1, rgb)
                        + fine.coverage(2 * x + 1, 2 * y + 1, rgb))
                        * 0.25
                }
            }
        })
        .collect();
    for (index, (gpu, cpu)) in gpu.iter().zip(&cpu).enumerate() {
        assert!(
            gpu.is_finite() || !cpu.is_finite(),
            "pixel {index}: the GPU's coverage is {gpu} where the CPU's is {cpu}"
        );
    }
    let differences: Vec<f64> = gpu.iter().zip(&cpu).map(|(g, c)| (g - c).abs()).collect();
    // A pixel whose coverage disagrees is a tie when the CPU's field, its samples moved by a step
    // of a finer stage, takes the GPU's value.
    let factor = if core.supersample {
        SUPERSAMPLED_FINE
    } else {
        POINT_FINE
    };
    let finer = CompiledMask::new(mask, stage(factor * width, factor * height), strokes)
        .expect("a finer mask");
    let mut ties = vec![false; differences.len()];
    let (mut tied, mut tie_moved) = (0, 0.0f64);
    for (index, difference) in differences.iter().enumerate() {
        if *difference <= 1e-3 {
            continue;
        }
        let at = (index as u32 % width, index as u32 / width);
        let rgb = pixels[index].map(f64::from);
        if let Some(moved) = tie(&finer, core.supersample, at, rgb, gpu[index]) {
            ties[index] = true;
            tied += 1;
            tie_moved = tie_moved.max(moved);
        }
    }
    let (contour, crossings) = contour_displacement(&cpu, &gpu, &ties, width, height);
    Measured {
        supersample: core.supersample,
        largest: differences.iter().copied().fold(0.0, f64::max),
        mean: differences.iter().sum::<f64>() / differences.len() as f64,
        contour: contour.max(tie_moved),
        crossings,
        ties: tied,
        tie_moved,
    }
}

/// The stages every kind is measured at: the corpus's Fit proxy of a 24 MP photograph, where a
/// thin feature takes the supersample, and an exact stage.
fn phases() -> [Phase; 2] {
    [
        Phase::Fit {
            proxy: stage(1716, 1144),
            full: stage(6000, 4000),
        },
        Phase::Exact(stage(1536, 1024)),
    ]
}

/// Every case of one kind at every phase: each held to the contour rule, the figures printed.
fn qualify(test: &str, cases: Vec<(&str, Mask, StrokeTable)>) {
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: {}", qualifier.adapter());
    let mut misses = Vec::new();
    for phase in phases() {
        let (_, pixels) = photograph(phase.stage().width, phase.stage().height);
        for (name, mask, strokes) in &cases {
            let measured = measure(&qualifier, mask, strokes, phase, &pixels);
            eprintln!(
                "{test}: {name}, {}{}: contour {:.4} px over {} crossings, coverage max {:.2e} \
                 mean {:.2e}, {} ties within {:.4} px",
                phase.name(),
                if measured.supersample {
                    ", supersampled"
                } else {
                    ""
                },
                measured.contour,
                measured.crossings,
                measured.largest,
                measured.mean,
                measured.ties,
                measured.tie_moved
            );
            assert!(
                measured.crossings > 0,
                "{name} at {}: the case draws no half-coverage contour",
                phase.name()
            );
            if measured.contour > 0.25 {
                misses.push(format!(
                    "{name} at {}: {:.4} px",
                    phase.name(),
                    measured.contour
                ));
            }
        }
    }
    assert!(
        misses.is_empty(),
        "the half-coverage contour lies more than a quarter pixel from the CPU's: {misses:?}"
    );
}

fn one(kind: &str, payload: Value) -> Mask {
    mask_of(&[(kind, ComponentMode::Add, false, payload)], false, 100.0)
}

#[test]
fn gpu_mask_linear_coverage_meets_the_contour_rule() {
    let none = StrokeTable::default;
    qualify(
        "gpu_mask_linear_coverage_meets_the_contour_rule",
        vec![
            (
                "the corpus's vertical gradient",
                one(
                    "linear",
                    json!({"x0": 0.5, "y0": 0.2, "x1": 0.5, "y1": 0.8}),
                ),
                none(),
            ),
            (
                "a rotated gradient",
                one(
                    "linear",
                    json!({"x0": 0.13, "y0": 0.91, "x1": 0.71, "y1": 0.17}),
                ),
                none(),
            ),
            (
                "a gradient from off the frame",
                one(
                    "linear",
                    json!({"x0": -1.0, "y0": 0.3, "x1": 2.0, "y1": 0.6}),
                ),
                none(),
            ),
            (
                "a ramp under two Fit pixels, which takes the supersample",
                one(
                    "linear",
                    json!({"x0": 0.4, "y0": 0.5, "x1": 0.4005, "y1": 0.5007}),
                ),
                none(),
            ),
            (
                "an inverted gradient at an amount",
                mask_of(
                    &[(
                        "linear",
                        ComponentMode::Add,
                        true,
                        json!({"x0": 0.2, "y0": 0.1, "x1": 0.8, "y1": 0.9}),
                    )],
                    false,
                    80.0,
                ),
                none(),
            ),
        ],
    );
}

#[test]
fn gpu_mask_radial_coverage_meets_the_contour_rule() {
    let none = StrokeTable::default;
    let radial = |x: f64, y: f64, rx: f64, ry: f64, angle: f64, feather: f64| json!({"x": x, "y": y, "radius_x": rx, "radius_y": ry, "angle": angle, "feather": feather});
    qualify(
        "gpu_mask_radial_coverage_meets_the_contour_rule",
        vec![
            (
                "the corpus's rotated, feathered ellipse",
                one("radial", radial(0.5, 0.5, 0.28, 0.22, 18.0, 45.0)),
                none(),
            ),
            (
                "a hard edge",
                one("radial", radial(0.43, 0.57, 0.31, 0.19, -37.0, 0.0)),
                none(),
            ),
            (
                "a small, thin ellipse",
                one("radial", radial(0.7, 0.3, 0.012, 0.04, 61.0, 10.0)),
                none(),
            ),
            (
                "a ramp to the centre",
                one("radial", radial(0.3, 0.6, 0.5, 0.35, 0.0, 100.0)),
                none(),
            ),
            (
                "an inverted ellipse in a whole-mask inversion",
                mask_of(
                    &[(
                        "radial",
                        ComponentMode::Add,
                        true,
                        radial(0.55, 0.45, 0.2, 0.3, 120.0, 30.0),
                    )],
                    true,
                    90.0,
                ),
                none(),
            ),
        ],
    );
}

/// A brush component of `strokes`, with the table its addresses resolve through.
fn brush(strokes: Vec<Stroke>) -> (Mask, StrokeTable) {
    let mut table = StrokeTable::new("the GPU mask tests");
    let addresses: Vec<String> = strokes
        .into_iter()
        .map(|stroke| table.insert(stroke).to_string())
        .collect();
    (one("brush", json!({ "strokes": addresses })), table)
}

#[test]
fn gpu_mask_brush_coverage_meets_the_contour_rule() {
    let corpus = brush(vec![
        Stroke::capture(
            &[[0.2, 0.4], [0.5, 0.6], [0.8, 0.4]],
            0.08,
            50.0,
            100.0,
            false,
        )
        .unwrap(),
    ]);
    let layered = brush(vec![
        Stroke::capture(
            &[[0.1, 0.2], [0.5, 0.25], [0.9, 0.6]],
            0.06,
            40.0,
            100.0,
            false,
        )
        .unwrap(),
        Stroke::capture(
            &[[0.15, 0.7], [0.45, 0.3], [0.85, 0.75]],
            0.05,
            0.0,
            70.0,
            false,
        )
        .unwrap(),
        Stroke::capture(&[[0.3, 0.1], [0.4, 0.9]], 0.03, 30.0, 100.0, true).unwrap(),
        Stroke::capture(&[[0.62, 0.55]], 0.09, 60.0, 85.0, false).unwrap(),
    ]);
    let limited = brush(vec![
        Stroke::capture(
            &[[0.05, 0.5], [0.5, 0.45], [0.95, 0.55]],
            0.25,
            30.0,
            100.0,
            false,
        )
        .unwrap()
        .with_colour_limit(ColourLimit::sampled([200, 120, 60], 35.0).unwrap()),
    ]);
    let thin = brush(vec![
        Stroke::capture(&[[0.2, 0.8], [0.8, 0.2]], 0.002, 0.0, 100.0, false).unwrap(),
    ]);
    qualify(
        "gpu_mask_brush_coverage_meets_the_contour_rule",
        vec![
            ("the corpus's feathered stroke", corpus.0, corpus.1),
            (
                "an erase over hard, feathered and one-point strokes",
                layered.0,
                layered.1,
            ),
            ("a stroke limited to a colour", limited.0, limited.1),
            ("a hard stroke under two Fit pixels wide", thin.0, thin.1),
        ],
    );
}

#[test]
fn gpu_mask_luminance_range_coverage_meets_the_contour_rule() {
    let none = StrokeTable::default;
    let band = |low: f64, low_feather: f64, high: f64, high_feather: f64| json!({"low": low, "low_feather": low_feather, "high": high, "high_feather": high_feather});
    qualify(
        "gpu_mask_luminance_range_coverage_meets_the_contour_rule",
        vec![
            (
                "the corpus's middle band",
                one("luminance-range", band(20.0, 10.0, 80.0, 10.0)),
                none(),
            ),
            (
                "a hard band",
                one("luminance-range", band(35.0, 0.0, 65.0, 0.0)),
                none(),
            ),
            (
                "a narrow band with one-unit shoulders",
                one("luminance-range", band(48.0, 1.0, 52.0, 1.0)),
                none(),
            ),
            (
                "the shadows, with a long shoulder",
                one("luminance-range", band(0.0, 0.0, 30.0, 45.0)),
                none(),
            ),
        ],
    );
}

#[test]
fn gpu_mask_colour_range_coverage_meets_the_contour_rule() {
    let none = StrokeTable::default;
    // Colours the photograph holds, each read where a pick on the canvas would read it.
    let (width, height) = (1716, 1144);
    let (_, pixels) = photograph(width, height);
    let pick = |u: f64, v: f64| {
        let (x, y) = (
            (u * f64::from(width)) as u32,
            (v * f64::from(height)) as u32,
        );
        pixels[(y * width + x) as usize].map(f64::from)
    };
    qualify(
        "gpu_mask_colour_range_coverage_meets_the_contour_rule",
        vec![
            (
                "the corpus's grey and warm samples",
                one(
                    "colour-range",
                    json!({"samples": [[0.18, 0.18, 0.18], [0.45, 0.2, 0.12]], "refine": 50.0}),
                ),
                none(),
            ),
            (
                "one picked blue at a tight refine",
                one(
                    "colour-range",
                    json!({"samples": [pick(0.62, 0.2)], "refine": 85.0}),
                ),
                none(),
            ),
            (
                "five picked colours at a loose refine",
                one(
                    "colour-range",
                    json!({"samples": [pick(0.3, 0.3), pick(0.7, 0.35), pick(0.5, 0.8),
                                       pick(0.15, 0.6), pick(0.85, 0.65)],
                           "refine": 45.0}),
                ),
                none(),
            ),
        ],
    );
}

/// The component algebra: an add, a subtract and an intersect, inverted components, a whole-mask
/// inversion and an amount, across geometric and value-based kinds.
#[test]
fn gpu_mask_composition_meets_the_contour_rule() {
    let none = StrokeTable::default;
    let linear = json!({"x0": 0.5, "y0": 0.2, "x1": 0.5, "y1": 0.8});
    let radial = json!({"x": 0.5, "y": 0.5, "radius_x": 0.28, "radius_y": 0.22, "angle": 18.0,
                        "feather": 45.0});
    let band = json!({"low": 20.0, "low_feather": 10.0, "high": 80.0, "high_feather": 10.0});
    let (strokes_mask, strokes) = brush(vec![
        Stroke::capture(&[[0.1, 0.3], [0.9, 0.35]], 0.07, 50.0, 100.0, false).unwrap(),
    ]);
    let mut with_brush = mask_of(
        &[(
            "radial",
            ComponentMode::Add,
            false,
            json!({"x": 0.3, "y": 0.6, "radius_x": 0.25, "radius_y": 0.25, "angle": 0.0,
                   "feather": 60.0}),
        )],
        false,
        100.0,
    );
    let mut stroke = strokes_mask.components[0].clone();
    stroke.mode = ComponentMode::Subtract;
    with_brush.components.push(stroke);
    qualify(
        "gpu_mask_composition_meets_the_contour_rule",
        vec![
            (
                "the corpus's linear less a radial",
                mask_of(
                    &[
                        ("linear", ComponentMode::Add, false, linear.clone()),
                        ("radial", ComponentMode::Subtract, false, radial.clone()),
                    ],
                    false,
                    100.0,
                ),
                none(),
            ),
            (
                "an inverted radial intersected with a luminance band, inverted at an amount",
                mask_of(
                    &[
                        ("radial", ComponentMode::Add, true, radial.clone()),
                        (
                            "luminance-range",
                            ComponentMode::Intersect,
                            false,
                            band.clone(),
                        ),
                        ("linear", ComponentMode::Add, false, linear.clone()),
                    ],
                    true,
                    75.0,
                ),
                none(),
            ),
            ("a radial less a brush stroke", with_brush, strokes),
        ],
    );
}

/// The value-based kinds, and a brush stroke limited to a colour, over the dense synthetic grid the
/// colour programs are qualified on: negative, in-range and over-range linear values and a half
/// float's extremes. A grid of independent pixels draws no contour to hold; what it holds the
/// programs to is the hard rule that a finite CPU coverage is never non-finite on the GPU, and it
/// reports how far the coverages differ there.
#[test]
fn gpu_mask_value_based_coverage_is_finite_over_extreme_inputs() {
    let test = "gpu_mask_value_based_coverage_is_finite_over_extreme_inputs";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let side = 256;
    let pixels = grid(side, side, 0x6d61_736b);
    let (limited, strokes) = brush(vec![
        Stroke::capture(&[[0.0, 0.5], [1.0, 0.5]], 2.0, 0.0, 100.0, false)
            .unwrap()
            .with_colour_limit(ColourLimit::sampled([200, 120, 60], 35.0).unwrap()),
    ]);
    let none = StrokeTable::default();
    for (name, mask, strokes) in [
        (
            "the corpus's luminance band",
            one(
                "luminance-range",
                json!({"low": 20.0, "low_feather": 10.0, "high": 80.0, "high_feather": 10.0}),
            ),
            &none,
        ),
        (
            "a hard luminance band",
            one(
                "luminance-range",
                json!({"low": 35.0, "low_feather": 0.0, "high": 65.0, "high_feather": 0.0}),
            ),
            &none,
        ),
        (
            "the corpus's colour range",
            one(
                "colour-range",
                json!({"samples": [[0.18, 0.18, 0.18], [0.45, 0.2, 0.12]], "refine": 50.0}),
            ),
            &none,
        ),
        (
            "a stroke over the frame limited to a colour",
            limited,
            &strokes,
        ),
    ] {
        let measured = measure(
            &qualifier,
            &mask,
            strokes,
            Phase::Exact(stage(side, side)),
            &pixels,
        );
        eprintln!(
            "{test}: {name}: coverage max {:.2e} mean {:.2e}, {} pixels at a tie",
            measured.largest, measured.mean, measured.ties
        );
    }
}

// ---- A painted stroke over 200 ticks ----------------------------------------------------------

/// One stroke painted over 200 ticks onto a brush component that already holds one, as a gesture
/// paints it: each tick the stroke holds every position posted so far, captured by the host, and
/// the owner plans the masked stack again. Every tick's storage block stays within the stroke
/// limits and keeps the committed stroke's segments where they were, a tick rewriting at most its
/// new segments and the index after them in the surface's 1 KiB chunks; at every tenth tick the GPU
/// frame meets the pointwise limits against the CPU frame of the same tick.
#[test]
fn gpu_mask_a_painted_stroke_over_200_ticks_stays_within_the_limits() {
    let test = "gpu_mask_a_painted_stroke_over_200_ticks_stays_within_the_limits";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let (width, height) = (1716u32, 1144u32);
    let (codes, pixels) = photograph(width, height);
    let source = SourceImage {
        width,
        height,
        rgba: Arc::new(codes),
        fingerprint: "sha256:gpu-mask-ticks".into(),
        orientation: 1,
        capture: Default::default(),
    };
    let held_boundary = boundary(width, height, 1, &pixels).expect("a boundary");
    let committed = Stroke::capture(
        &[[0.1, 0.8], [0.5, 0.75], [0.9, 0.85]],
        0.05,
        40.0,
        100.0,
        false,
    )
    .unwrap();
    // A hand's path: a slow curve across the frame with a wobble, one position a tick.
    let path: Vec<[f64; 2]> = (0..=200)
        .map(|tick| {
            let t = f64::from(tick) / 200.0;
            [
                0.1 + 0.8 * t,
                0.45 + 0.2 * (t * 5.0).sin() + 0.01 * (t * 61.0).sin(),
            ]
        })
        .collect();
    let registry = ModuleRegistry::builtin();
    let context = RenderContext::new();
    let (mut previous, mut written, mut total, mut largest) =
        (None::<(Arc<[u32]>, usize)>, 0, 0, 0);
    let (mut appended, mut redecimated) = (0, 0);
    let mut sampled = Vec::new();
    for tick in 1..=200 {
        let painted = Stroke::capture(&path[..=tick], 0.04, 50.0, 80.0, false).unwrap();
        let (mask, strokes) = brush(vec![committed.clone(), painted]);
        let recipe = masked_recipe(&mask, &strokes);
        let plan = planned(
            &recipe,
            GpuPlanRequest::exact(0, stage(width, height)).qualifying(),
        );
        let converted = surface_plan(&plan, held_boundary.clone()).expect("a runnable plan");
        let GpuStep::Masked(step) = &converted.steps[0] else {
            panic!("a masked step");
        };
        let block = Arc::clone(&step.mask.components[0].program.block);
        // The segments are the block's first part, up to the cell table's offset (word 6).
        let segments = step.mask.components[0].program.words[6] as usize;
        assert!(
            block.len() <= BRUSH_GPU_BLOCK_WORDS_MAX,
            "tick {tick}: {} words",
            block.len()
        );
        largest = largest.max(block.len());
        total += block.len();
        if let Some((previous, held_segments)) = &previous {
            // The committed stroke's segments, its first part, are where they were.
            let committed_words = 5 * 2;
            assert_eq!(
                block[..committed_words],
                previous[..committed_words],
                "tick {tick}"
            );
            // A tick appends the stroke's new segments, unless the host's decimation of the longer
            // path moved a position it had already kept.
            let first = block
                .iter()
                .zip(previous.iter())
                .position(|(new, old)| new != old)
                .unwrap_or(block.len().min(previous.len()));
            if first < *held_segments {
                redecimated += 1;
            } else {
                appended += 1;
            }
            written += (0..block.len())
                .step_by(256)
                .filter(|&start| {
                    let end = (start + 256).min(block.len());
                    previous.get(start..end) != Some(&block[start..end])
                })
                .map(|start| (start + 256).min(block.len()) - start)
                .sum::<usize>();
        } else {
            written += block.len();
        }
        previous = Some((block, segments));
        if tick % 10 == 0 || tick == 1 {
            let cpu = render(
                &registry,
                &source,
                &recipe,
                RenderOptions::exact(&Cancel::never()),
                &context,
            )
            .and_then(|render| render.frame(SnapshotId::new()))
            .expect("the CPU frame");
            let reference: Vec<u8> = cpu
                .rgba
                .chunks_exact(4)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect();
            let drawn: Vec<u8> = qualifier
                .evaluate_codes(&converted)
                .expect("the GPU frame")
                .iter()
                .flat_map(|code| [code[0], code[1], code[2]])
                .collect();
            let frame = |bytes| Rgb8::new(width, height, bytes).expect("a frame");
            let statistics =
                preview_error::compare(frame(&drawn), frame(&reference), [0, 0, width, height])
                    .expect("statistics");
            assert!(
                preview_error::verdict(&statistics, Class::Pointwise).passed(),
                "tick {tick}: {}",
                figures(&statistics)
            );
            sampled.push(statistics);
        }
    }
    let worst: Statistics = worst(&sampled);
    eprintln!(
        "{test}: {} sampled ticks, worst {}; blocks {} words at most (bound {}), {} of {} words \
         written in 1 KiB chunks over 200 ticks; {appended} ticks appended to the segments and \
         {redecimated} moved a kept position",
        sampled.len(),
        figures(&worst),
        largest,
        BRUSH_GPU_BLOCK_WORDS_MAX,
        written,
        total
    );
}

// ---- The corpus -------------------------------------------------------------------------------

/// The corpus's masked colour recipes at Fit, through the shared harness: every mask kind and the
/// component algebra carrying a masked Basic layer, each held to the pointwise limits.
#[test]
#[ignore = "the GPU preview corpus at Fit: set LUXFORGE_GPU_CORPUS_OUTPUT to a new directory, \
            LUXFORGE_GENERATED_FIXTURES to the generated JPEGs and, for the RAWs, \
            LUXFORGE_RAW_MANIFEST"]
fn gpu_mask_corpus_at_fit() {
    corpus_at_fit(
        "gpu_mask_corpus_at_fit",
        &[
            "mask-linear",
            "mask-radial",
            "mask-brush",
            "mask-luminance-range",
            "mask-colour-range",
            "mask-composed",
        ],
    );
}

/// A painted stroke's ticks evaluated incrementally, as one slot draws a gesture's: each tick's
/// plan changes only near the segments it adds, removes or moves (`GpuPlan::changes_since`), so
/// the surface evaluates each link of the chain only where that change reaches and its mask can
/// cover anything, and keeps the rest of what its intermediates and planes hold from the ticks
/// before — through a masked Basic layer, a masked Presence layer of the same mask, whose bounds
/// grow with the stroke, Presence layers masked by radials the stroke passes near and far from,
/// a global Presence layer after them and a masked Basic layer after that, which the plan draws
/// through an identity tail, at a Fit stage's thin-feature scale. The stroke is painted to its
/// end, taken back over half of it and painted to its end again, so the brushed mask grows,
/// shrinks and grows again. Every tick's frame is the frame a whole evaluation of the same plan
/// draws, bit for bit, and the rectangles are a small part of the stage. So it is again with the
/// poison on, every link's passes starting from NaN in every texture of the pool the links share
/// ([`held_to_whole`]).
#[test]
fn gpu_mask_a_painted_stroke_is_evaluated_where_each_tick_changes_it() {
    let test = "gpu_mask_a_painted_stroke_is_evaluated_where_each_tick_changes_it";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let (width, height) = (960u32, 640u32);
    let (_, pixels) = photograph(width, height);
    let held_boundary = boundary(width, height, 1, &pixels).expect("a boundary");
    let committed = Stroke::capture(
        &[[0.1, 0.8], [0.5, 0.75], [0.9, 0.85]],
        0.05,
        40.0,
        100.0,
        false,
    )
    .unwrap();
    let path: Vec<[f64; 2]> = (0..=40)
        .map(|tick| {
            let t = f64::from(tick) / 40.0;
            [
                0.15 + 0.7 * t,
                0.45 + 0.2 * (t * 5.0).sin() + 0.01 * (t * 61.0).sin(),
            ]
        })
        .collect();
    // The same stroke whose second half takes another route a little lower, inside the first's
    // bounds.
    let rerouted: Vec<[f64; 2]> = path
        .iter()
        .enumerate()
        .map(|(tick, [x, y])| if tick > 20 { [*x, y + 0.03] } else { [*x, *y] })
        .collect();
    let presence = |payload: Value| Layer::new(luxforge_core::PRESENCE_EFFECT, payload);
    // Radials the stroke passes near and far from, each with a Presence layer of its own.
    let radials: Vec<Mask> = [(0.55, 0.35), (0.12, 0.15)]
        .iter()
        .enumerate()
        .map(|(index, (x, y))| {
            let mut mask = mask_of(
                &[(
                    "radial",
                    ComponentMode::Add,
                    false,
                    json!({"x": x, "y": y, "radius_x": 0.08, "radius_y": 0.06, "angle": 0.0,
                           "feather": 40.0}),
                )],
                false,
                100.0,
            );
            mask.name = format!("Radial {index}");
            mask
        })
        .collect();
    let stack = |(path, tick): (&[[f64; 2]], usize)| {
        let painted = Stroke::capture(&path[..=tick], 0.04, 50.0, 80.0, false).unwrap();
        let (mask, strokes) = brush(vec![committed.clone(), painted]);
        let mut recipe = masked_recipe(&mask, &strokes);
        recipe.layers.push(Layer {
            mask: Some(mask.id.clone()),
            ..presence(json!({"clarity": 50.0, "texture": 40.0}))
        });
        for radial in &radials {
            recipe.layers.push(Layer {
                mask: Some(radial.id.clone()),
                ..presence(json!({"clarity": -40.0, "texture": 30.0}))
            });
            recipe.masks.push(radial.clone());
        }
        recipe
            .layers
            .push(presence(json!({"clarity": 20.0, "texture": 25.0})));
        // A colour layer after the last spatial one, which the plan runs after its geometry: an
        // identity tail, which quantizes on a JPEG.
        recipe.layers.push(Layer {
            mask: Some(radials[0].id.clone()),
            ..basic()
        });
        recipe
    };
    let full = stage(width * 4, height * 4);
    let request = GpuPlanRequest::fit(0, stage(width, height), full).qualifying();
    // The stroke painted to its end, taken back over half of it, as an erase shrinks a mask, and
    // painted to its end again: one slot's ticks, each from the one before. Then from a slot that
    // first drew the whole stroke, whose brushed layer holds its planes only where its mask needs
    // them while the stroke is taken back and painted again by another route within its bounds.
    let mut areas = Vec::new();
    let forward = |path, ticks: &mut dyn Iterator<Item = usize>| -> Vec<(&[[f64; 2]], usize)> {
        ticks.map(|tick| (path, tick)).collect()
    };
    for order in [
        [
            forward(&path, &mut (1..=40)),
            forward(&path, &mut (20..40).rev()),
            forward(&path, &mut (21..=40)),
        ]
        .concat(),
        [
            forward(&path, &mut (20..=40).rev()),
            forward(&rerouted, &mut (21..=40)),
        ]
        .concat(),
    ] {
        let plans: Vec<_> = order
            .iter()
            .map(|&tick| planned(&stack(tick), request))
            .collect();
        let ticks: Vec<_> = plans
            .iter()
            .enumerate()
            .map(|(number, plan)| {
                let inside = (number > 0).then(|| match plan.changes_since(&plans[number - 1]) {
                    luxforge_core::GpuChange::Inside(rect) => {
                        areas.push(f64::from(rect.width * rect.height) / f64::from(width * height));
                        [
                            rect.x0,
                            rect.y0,
                            rect.x0 + rect.width,
                            rect.y0 + rect.height,
                        ]
                    }
                    luxforge_core::GpuChange::Nothing => [0; 4],
                    luxforge_core::GpuChange::Anywhere => {
                        panic!("tick {number}: a change anywhere")
                    }
                });
                let converted = surface_plan(plan, held_boundary.clone()).expect("a runnable plan");
                assert!(
                    converted.steps.iter().any(|step| matches!(
                        step,
                        luxforge_ui::photo_surface::GpuStep::Geometry(_)
                    )),
                    "the plan draws through its tail"
                );
                (converted, inside)
            })
            .collect();
        let whole: Vec<_> = ticks
            .iter()
            .map(|(plan, _)| qualifier.evaluate(plan).expect("a readback"))
            .collect();
        held_to_whole(&qualifier, &ticks, &whole, &|number| {
            format!(
                "tick {number} (stroke position {}) over {:?}",
                order[number].1, ticks[number].1
            )
        });
    }
    let mean = areas.iter().sum::<f64>() / areas.len() as f64;
    eprintln!(
        "{test}: {} ticks, each changing on average {:.1}% of the stage (largest {:.1}%)",
        areas.len(),
        100.0 * mean,
        100.0 * areas.iter().copied().fold(0.0, f64::max)
    );
    assert!(mean < 0.25, "the change stays near the stroke's end");
}

/// A radial mask moved, as dragging its handle moves it, changes coverage only inside its bounds
/// before and after the move (`GpuPlan::changes_since`): its ticks are evaluated incrementally,
/// as one slot draws them, through a masked Basic and a masked Presence layer of the radial and a
/// global Presence layer after them, at a Fit stage's thin-feature scale, and its amount and an
/// inversion changed by the same rule, the inverted mask's bounds the whole stage. Every tick's
/// frame is the frame a whole evaluation of the same plan draws, bit for bit, with the poison on
/// too ([`held_to_whole`]), and a moved radial's change is the part of the stage the two positions
/// cover.
#[test]
fn gpu_mask_a_moved_radial_is_evaluated_where_its_bounds_were_and_are() {
    let test = "gpu_mask_a_moved_radial_is_evaluated_where_its_bounds_were_and_are";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let (width, height) = (960u32, 640u32);
    let (_, pixels) = photograph(width, height);
    let held_boundary = boundary(width, height, 1, &pixels).expect("a boundary");
    let presence = |payload: Value| Layer::new(luxforge_core::PRESENCE_EFFECT, payload);
    let stack = |x: f64, amount: f64, invert: bool| {
        let mask = mask_of(
            &[(
                "radial",
                ComponentMode::Add,
                false,
                json!({"x": x, "y": 0.45, "radius_x": 0.12, "radius_y": 0.1, "angle": 20.0,
                       "feather": 50.0}),
            )],
            invert,
            amount,
        );
        let mut recipe = masked_recipe(&mask, &StrokeTable::default());
        recipe.layers.push(Layer {
            mask: Some(mask.id.clone()),
            ..presence(json!({"clarity": 50.0, "texture": 40.0}))
        });
        recipe
            .layers
            .push(presence(json!({"clarity": 20.0, "texture": 25.0})));
        recipe
    };
    let full = stage(width * 4, height * 4);
    let request = GpuPlanRequest::fit(0, stage(width, height), full).qualifying();
    let order: Vec<(f64, f64, bool)> = (0..24)
        .map(|tick| (0.3 + 0.01 * f64::from(tick), 100.0, false))
        .chain([(0.53, 60.0, false), (0.53, 60.0, true), (0.55, 60.0, true)])
        .collect();
    let plans: Vec<_> = order
        .iter()
        .map(|&(x, amount, invert)| planned(&stack(x, amount, invert), request))
        .collect();
    let mut moved = Vec::new();
    let ticks: Vec<_> = plans
        .iter()
        .enumerate()
        .map(|(number, plan)| {
            let inside = (number > 0).then(|| match plan.changes_since(&plans[number - 1]) {
                luxforge_core::GpuChange::Inside(rect) => {
                    if number < 24 {
                        moved.push(f64::from(rect.width * rect.height) / f64::from(width * height));
                    }
                    [
                        rect.x0,
                        rect.y0,
                        rect.x0 + rect.width,
                        rect.y0 + rect.height,
                    ]
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
    held_to_whole(&qualifier, &ticks, &whole, &|number| {
        format!(
            "tick {number} {:?} over {:?}",
            order[number], ticks[number].1
        )
    });
    let mean = moved.iter().sum::<f64>() / moved.len() as f64;
    eprintln!(
        "{test}: {} moves, each changing on average {:.1}% of the stage",
        moved.len(),
        100.0 * mean
    );
    assert!(mean < 0.25, "a move changes about the radial's own area");
}

/// A masked Presence layer whose mask lies nearer the boundary's left edge than its passes reach,
/// so its pass rectangle runs out to that edge, after a masked Basic layer painted along that edge:
/// the Basic stroke changes the Presence layer's input between its mask and the edge, then the
/// Presence layer's own mask is painted toward the edge, inside the rectangle it already had, as
/// one slot draws the ticks. Every tick's frame is the frame a whole evaluation of the same plan
/// draws, bit for bit, with the poison on too ([`held_to_whole`]): where the mask grew, its apply
/// reads the Presence values of the input the Basic stroke left, not of the one before it.
#[test]
fn gpu_mask_a_mask_grown_toward_the_edge_reads_its_inputs_latest_values() {
    let test = "gpu_mask_a_mask_grown_toward_the_edge_reads_its_inputs_latest_values";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    let (width, height) = (960u32, 640u32);
    let (_, pixels) = photograph(width, height);
    let held_boundary = boundary(width, height, 1, &pixels).expect("a boundary");
    let presence = |payload: Value| Layer::new(luxforge_core::PRESENCE_EFFECT, payload);
    // The Presence mask's committed stroke and the stroke that paints it toward the left edge, and
    // the Basic mask's stroke down that edge.
    let committed = Stroke::capture(&[[0.14, 0.5], [0.4, 0.55]], 0.04, 50.0, 100.0, false).unwrap();
    let down: Vec<[f64; 2]> = (0..=10)
        .map(|tick| [0.015, 0.3 + 0.04 * f64::from(tick)])
        .collect();
    let toward: Vec<[f64; 2]> = (0..=10)
        .map(|tick| [0.14 - 0.012 * f64::from(tick), 0.52])
        .collect();
    let stack = |edge: usize, grown: usize| {
        let mut strokes = StrokeTable::new(test);
        let mut brushed = |painted: Vec<Stroke>| {
            let addresses: Vec<String> = painted
                .into_iter()
                .map(|stroke| strokes.insert(stroke).to_string())
                .collect();
            one("brush", json!({ "strokes": addresses }))
        };
        let along = brushed(vec![
            Stroke::capture(&down[..=edge], 0.03, 30.0, 100.0, false).unwrap(),
        ]);
        let mut presence_strokes = vec![committed.clone()];
        if grown > 0 {
            presence_strokes
                .push(Stroke::capture(&toward[..=grown], 0.04, 50.0, 100.0, false).unwrap());
        }
        let grown_mask = brushed(presence_strokes);
        Recipe {
            layers: vec![
                Layer {
                    mask: Some(along.id.clone()),
                    ..basic()
                },
                Layer {
                    mask: Some(grown_mask.id.clone()),
                    ..presence(json!({"clarity": 60.0, "texture": 40.0}))
                },
            ],
            masks: vec![along, grown_mask],
            strokes,
            ..Recipe::default()
        }
    };
    let full = stage(width * 4, height * 4);
    let request = GpuPlanRequest::fit(0, stage(width, height), full).qualifying();
    // The Basic stroke painted down the edge, then the Presence stroke toward it.
    let order: Vec<(usize, usize)> = (1..=10)
        .map(|edge| (edge, 0))
        .chain((1..=10).map(|grown| (10, grown)))
        .collect();
    let plans: Vec<_> = order
        .iter()
        .map(|&(edge, grown)| planned(&stack(edge, grown), request))
        .collect();
    let ticks: Vec<_> = plans
        .iter()
        .enumerate()
        .map(|(number, plan)| {
            let inside = (number > 0).then(|| match plan.changes_since(&plans[number - 1]) {
                luxforge_core::GpuChange::Inside(rect) => [
                    rect.x0,
                    rect.y0,
                    rect.x0 + rect.width,
                    rect.y0 + rect.height,
                ],
                luxforge_core::GpuChange::Nothing => [0; 4],
                luxforge_core::GpuChange::Anywhere => panic!("tick {number}: a change anywhere"),
            });
            let converted = surface_plan(plan, held_boundary.clone()).expect("a runnable plan");
            (converted, inside)
        })
        .collect();
    // The case this holds: a pass rectangle out to the left edge around a mask that does not reach
    // it, and that mask grown toward it inside the rectangle.
    let masked = |plan: &GpuPlan| {
        let spatial = plan
            .steps
            .iter()
            .find_map(|step| match step {
                GpuStep::Spatial(spatial) if spatial.mask.is_some() => Some(spatial),
                _ => None,
            })
            .expect("a masked Presence step");
        let size = plan.boundary.size();
        (
            spatial.pass_rect(plan.texels, size),
            spatial.mask_rect(plan.texels, size, 0),
        )
    };
    let (first_rect, first_bounds) = masked(&ticks[0].0);
    let (last_rect, last_bounds) = masked(&ticks[ticks.len() - 1].0);
    assert!(
        first_rect.x0 == 0 && first_bounds.x0 > 0 && last_rect.x0 == 0,
        "the pass rectangle {first_rect:?} reaches the edge around the mask's {first_bounds:?}"
    );
    assert!(
        last_bounds.x0 < first_bounds.x0,
        "the mask grew toward the edge: {first_bounds:?} to {last_bounds:?}"
    );
    let whole: Vec<_> = ticks
        .iter()
        .map(|(plan, _)| qualifier.evaluate(plan).expect("a readback"))
        .collect();
    held_to_whole(&qualifier, &ticks, &whole, &|number| {
        format!(
            "tick {number} {:?} over {:?}",
            order[number], ticks[number].1
        )
    });
}

/// What a painted stroke's ticks cost the GPU, evaluated incrementally and whole: the stroke the
/// editor-latency paint mode paints, on a brushed mask and two radials, each mask holding a masked
/// Basic exposure and a masked Presence layer, at a Fit proxy's stage and at a 100% region's,
/// over this host's adapter. Every tick after the first is submitted without waiting, as a slot's
/// are, so the figure is the GPU's throughput a tick and the CPU's encoding beside it, after the
/// time the chain's first two ticks took to compile and draw. A functional measurement for
/// `docs/design/gpu-preview.md`'s incremental ticks, not a timing gate: run it on purpose, with
/// `--ignored --nocapture`, and record the build profile and host beside its figures.
#[test]
#[ignore = "a measurement, run on purpose"]
fn gpu_mask_a_painted_stroke_costs_where_it_changes() {
    let test = "gpu_mask_a_painted_stroke_costs_where_it_changes";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let path: Vec<[f64; 2]> = (0..120)
        .map(|index| {
            let t = f64::from(index) / 119.0;
            [
                0.2 + 0.8 * t,
                0.5 + 0.2 * (5.0 * std::f64::consts::PI * t).sin(),
            ]
        })
        .collect();
    let radials: Vec<Mask> = [0.35, 0.7]
        .iter()
        .enumerate()
        .map(|(index, x)| {
            let mut mask = mask_of(
                &[(
                    "radial",
                    ComponentMode::Add,
                    false,
                    json!({"x": x, "y": 0.3, "radius_x": 0.2, "radius_y": 0.16, "angle": 0.0,
                           "feather": 50.0}),
                )],
                false,
                100.0,
            );
            mask.name = format!("Radial {index}");
            mask
        })
        .collect();
    let stack = |tick: usize| {
        let painted = Stroke::capture(&path[..=tick], 0.06, 50.0, 100.0, false).unwrap();
        let (brushed, strokes) = brush(vec![painted]);
        let mut recipe = Recipe {
            strokes,
            ..Recipe::default()
        };
        for mask in std::iter::once(&brushed).chain(&radials) {
            recipe.layers.push(Layer {
                mask: Some(mask.id.clone()),
                ..basic()
            });
            recipe.layers.push(Layer {
                mask: Some(mask.id.clone()),
                ..Layer::new(
                    luxforge_core::PRESENCE_EFFECT,
                    json!({"clarity": 50.0, "texture": 40.0}),
                )
            });
            recipe.masks.push(mask.clone());
        }
        recipe
    };
    let full = stage(6000, 4000);
    for (width, height) in [(2292, 1528), (2994, 2642)] {
        let pixels: Vec<[f32; 3]> = (0..width * height)
            .map(|index| {
                let (x, y) = (index % width, index / width);
                let v = ((x * 7 + y * 13) % 255) as f32 / 255.0;
                [v * 0.8, 0.3 + v * 0.4, 0.5 - v * 0.3]
            })
            .collect();
        let held_boundary = boundary(width, height, 1, &pixels).expect("a boundary");
        let request = GpuPlanRequest::fit(0, stage(width, height), full);
        let plans: Vec<_> = (1..path.len())
            .map(|tick| planned(&stack(tick), request))
            .collect();
        let mut shares = Vec::new();
        let ticks: Vec<_> = plans
            .iter()
            .enumerate()
            .map(|(tick, plan)| {
                let inside = (tick > 0).then(|| match plan.changes_since(&plans[tick - 1]) {
                    luxforge_core::GpuChange::Inside(rect) => {
                        shares
                            .push(f64::from(rect.width * rect.height) / f64::from(width * height));
                        [
                            rect.x0,
                            rect.y0,
                            rect.x0 + rect.width,
                            rect.y0 + rect.height,
                        ]
                    }
                    luxforge_core::GpuChange::Nothing => [0; 4],
                    luxforge_core::GpuChange::Anywhere => panic!("tick {tick}: a change anywhere"),
                });
                let converted = surface_plan(plan, held_boundary.clone()).expect("a runnable plan");
                (converted, inside)
            })
            .collect();
        let whole: Vec<_> = ticks.iter().map(|(plan, _)| (plan.clone(), None)).collect();
        // Once to compile the chain's pipelines, then measured.
        let compiled = std::time::Instant::now();
        qualifier
            .time_throughput(&ticks[..2])
            .expect("a compiled chain");
        let compiled = compiled.elapsed();
        let (encoding, incremental) = qualifier.time_throughput(&ticks).expect("a timing");
        let (whole_encoding, wholly) = qualifier.time_throughput(&whole).expect("a timing");
        let mean = shares.iter().sum::<f64>() / shares.len() as f64;
        eprintln!(
            "{test}: {width}x{height}: the chain compiled and drew its first ticks in {:.0} ms; \
             each tick changes {:.1}% of the stage on average; incremental {:.2} ms a tick \
             (encoding {:.2} ms), whole {:.2} ms (encoding {:.2} ms)",
            compiled.as_secs_f64() * 1e3,
            100.0 * mean,
            incremental.as_secs_f64() * 1e3,
            encoding.as_secs_f64() * 1e3,
            wholly.as_secs_f64() * 1e3,
            whole_encoding.as_secs_f64() * 1e3,
        );
    }
}

/// What a painted stroke's ticks cost the GPU at the Fit stage of `editor-latency --mode paint
/// --masks N --mask-presence` on the generated 24 MP JPEG in its 1728 × 1080 window (2292 × 1528 of
/// a 6000 × 4000 photograph), in the harness's own layout: the brushed mask, its seeding stroke
/// committed and the measured stroke painted into the same component one position a tick, and
/// each further mask a radial placed as the harness places it; each mask holding a masked Basic
/// exposure of +0.6 and a masked Presence layer of Clarity 50 and Texture 40, placement putting
/// every masked Basic layer before every masked Presence layer. Each tick changes the first
/// mask's Basic layer near the stroke's end, so every Presence link after it is evaluated where
/// that change reaches and its mask can cover anything. For 3, 10 and 16 masks it prints the
/// slot's charge, the share of the stage a tick changes, the GPU's throughput a tick evaluated
/// incrementally and whole, with the CPU's encoding beside each, after the chain's first two
/// ticks compiled and drew, and how long each incremental tick alone keeps the GPU, from its
/// submission to its completion. A measurement, not a timing gate: run it on purpose in release,
/// with `--ignored --nocapture`, on a quiet host, and record the load beside its figures.
#[test]
#[ignore = "a measurement, run on purpose"]
fn gpu_mask_the_paint_harness_layout_costs_a_tick_at_fit() {
    let test = "gpu_mask_the_paint_harness_layout_costs_a_tick_at_fit";
    let Some(qualifier) = super::gpu_qualification::headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let seed = Stroke::capture(&[[0.2, 0.3], [0.8, 0.3]], 0.06, 50.0, 100.0, false).unwrap();
    let path: Vec<[f64; 2]> = (0..120)
        .map(|index| {
            let t = f64::from(index) / 119.0;
            [
                0.2 + 0.8 * t,
                0.5 + 0.2 * (std::f64::consts::TAU * 2.5 * t).sin(),
            ]
        })
        .collect();
    let (width, height) = (2292u32, 1528u32);
    let pixels: Vec<[f32; 3]> = (0..width * height)
        .map(|index| {
            let (x, y) = (index % width, index / width);
            let v = ((x * 7 + y * 13) % 255) as f32 / 255.0;
            [v * 0.8, 0.3 + v * 0.4, 0.5 - v * 0.3]
        })
        .collect();
    let held_boundary = boundary(width, height, 1, &pixels).expect("a boundary");
    let request = GpuPlanRequest::fit(0, stage(width, height), stage(6000, 4000));
    for masks in [3usize, 10, 16] {
        let radials: Vec<Mask> = (2..=masks)
            .map(|index| {
                let t = (index - 2) as f64 / (masks.max(3) - 2) as f64;
                let mut mask = mask_of(
                    &[(
                        "radial",
                        ComponentMode::Add,
                        false,
                        json!({"x": 0.15 + 0.7 * t, "y": 0.35 + 0.3 * (t * 7.0).sin().abs(),
                               "radius_x": 0.18, "radius_y": 0.14, "angle": 0.0,
                               "feather": 50.0}),
                    )],
                    false,
                    100.0,
                );
                mask.name = format!("Mask {index}");
                mask
            })
            .collect();
        let stack = |tick: usize| {
            let painted = Stroke::capture(&path[..=tick], 0.06, 50.0, 100.0, false).unwrap();
            let (brushed, strokes) = brush(vec![seed.clone(), painted]);
            let masks: Vec<Mask> = std::iter::once(brushed).chain(radials.clone()).collect();
            let masked = |mask: &Mask, effect: &str, payload: Value| Layer {
                mask: Some(mask.id.clone()),
                ..Layer::new(effect, payload)
            };
            let mut layers: Vec<Layer> = masks
                .iter()
                .map(|mask| masked(mask, BASIC_EFFECT, json!({"exposure": 0.6})))
                .collect();
            layers.extend(masks.iter().map(|mask| {
                masked(
                    mask,
                    luxforge_core::PRESENCE_EFFECT,
                    json!({"clarity": 50.0, "texture": 40.0}),
                )
            }));
            Recipe {
                layers,
                masks,
                strokes,
                ..Recipe::default()
            }
        };
        let plans: Vec<_> = (1..path.len())
            .map(|tick| planned(&stack(tick), request))
            .collect();
        assert_eq!(plans[0].spatial.len(), masks, "a Presence link a mask");
        assert_eq!(plans[0].content.len(), masks, "the Basic layers first");
        let mut shares = Vec::new();
        let ticks: Vec<_> = plans
            .iter()
            .enumerate()
            .map(|(tick, plan)| {
                let inside = (tick > 0).then(|| match plan.changes_since(&plans[tick - 1]) {
                    luxforge_core::GpuChange::Inside(rect) => {
                        shares
                            .push(f64::from(rect.width * rect.height) / f64::from(width * height));
                        [
                            rect.x0,
                            rect.y0,
                            rect.x0 + rect.width,
                            rect.y0 + rect.height,
                        ]
                    }
                    luxforge_core::GpuChange::Nothing => [0; 4],
                    luxforge_core::GpuChange::Anywhere => panic!("tick {tick}: a change anywhere"),
                });
                let converted = surface_plan(plan, held_boundary.clone()).expect("a runnable plan");
                (converted, inside)
            })
            .collect();
        let charged = qualifier.charged_bytes(&ticks[0].0).expect("a charge");
        let whole: Vec<_> = ticks.iter().map(|(plan, _)| (plan.clone(), None)).collect();
        // Once to compile the chain's pipelines, then measured.
        let compiled = std::time::Instant::now();
        qualifier
            .time_throughput(&ticks[..2])
            .expect("a compiled chain");
        let compiled = compiled.elapsed();
        let (encoding, incremental) = qualifier.time_throughput(&ticks).expect("a timing");
        let (whole_encoding, wholly) = qualifier.time_throughput(&whole).expect("a timing");
        // Each incremental tick alone, from its submission to its completion after the tick
        // before it was drawn whole and waited for: how long one tick's work keeps the GPU.
        let mut alone: Vec<f64> = (1..ticks.len())
            .map(|tick| {
                let (_, wall) = qualifier
                    .time_throughput(&ticks[tick - 1..=tick])
                    .expect("a timing");
                wall.as_secs_f64() * 1e3
            })
            .collect();
        alone.sort_by(f64::total_cmp);
        shares.sort_by(f64::total_cmp);
        let at = |values: &[f64], share: usize| values[(values.len() - 1) * share / 100];
        let mean = shares.iter().sum::<f64>() / shares.len() as f64;
        eprintln!(
            "{test}: {masks} masks at {width}x{height}: the slot charges {:.1} MB; the chain \
             compiled and drew its first ticks in {:.0} ms; each of {} ticks changes {:.1}% of \
             the stage on average (p50 {:.1}%, p95 {:.1}%, largest {:.1}%); incremental {:.2} ms \
             a tick (encoding {:.2} ms), whole {:.2} ms (encoding {:.2} ms); one incremental \
             tick alone p50 {:.2} ms, p95 {:.2} ms, largest {:.2} ms",
            charged as f64 / 1e6,
            compiled.as_secs_f64() * 1e3,
            ticks.len(),
            100.0 * mean,
            100.0 * at(&shares, 50),
            100.0 * at(&shares, 95),
            100.0 * at(&shares, 100),
            incremental.as_secs_f64() * 1e3,
            encoding.as_secs_f64() * 1e3,
            wholly.as_secs_f64() * 1e3,
            whole_encoding.as_secs_f64() * 1e3,
            at(&alone, 50),
            at(&alone, 95),
            at(&alone, 100),
        );
    }
}
