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
use super::gpu_qualification::{corpus_at_fit, figures, grid, worst};
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
