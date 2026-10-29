//! The grain module's GPU program (`docs/design/grain.md`, `docs/design/grain-study.md`) on this
//! host's device, against the study's independent `f64` reference and the CPU unit it mirrors:
//!
//! - **The hash, exactly.** The program's own finaliser, component keys, hashes, lattice values and
//!   lattice split, run on the device, equal the study's frozen vectors and the reference's integer
//!   arithmetic bit for bit, over a dense sweep of positions too.
//! - **Full resolution.** A grain layer over the dense synthetic grid (in range, past white, below
//!   black and the extremes), at the slider corners, the defaults and a small amount, near the
//!   origin and over a window whose positions cross `2^16`, meets the pointwise limits against the
//!   reference and the CPU unit, with nothing non-finite.
//! - **Windows and tiles.** Under a straightened crop, where the program runs in output space after
//!   the tail, a 100% view's window and the tiles of the picture at rest draw the whole output
//!   stage's frame bit for bit, which meets the pointwise limits against the reference renderer's.
//! - **A reduced stage.** At Fit's reduced stages (footprints 2, 4 and 8, and a non-integer one) the
//!   program draws the study's reduced-stage field: within the pointwise limits of the reference
//!   at that footprint, and over flat greys within the study's statistical tolerance of the
//!   full-resolution grain reduced by its linear-light area average.
//! - **A neutral draft.** A drafted Amount 0 layer's GPU shape is the exact identity.
//!
//! A test with no adapter prints that it was skipped and asserts nothing: the skip is the report,
//! and `cargo test` counting it as passed does not make it GPU evidence.
use super::gpu_plan::{surface_plan, surface_plan_at, surface_plan_over};
use super::gpu_qualification::{codes, figures, grid, headless};
use super::gpu_window_tests::{HEIGHT, WIDTH, cut, source, whole};
use luxforge_core::{
    BASIC_EFFECT, BoundaryFormat, Cancel, CompileStage, GPU_PROGRAMS, GRAIN_EFFECT, GpuAnswer,
    GpuPlanRequest, Layer, ModuleRegistry, PointwiseColor, Processing, Recipe, Region,
    RenderContext, RenderOptions, SnapshotId, Stage, anchored, gpu_plan, render,
};
use luxforge_gpu::{
    GpuPlan, GpuProgram, GpuStep, TexelMap,
    qualification::{boundary, boundary_as, held},
};
use luxforge_reference::grain as reference;
use luxforge_reference::preview_error::{self, Class, Rgb8, Statistics};
use luxforge_reference::srgb;
use luxforge_test_macros::slow;
use serde_json::{Value, json};
use std::sync::Arc;

const ENTRY: &str = "lf_grain_film";

fn registry() -> ModuleRegistry {
    ModuleRegistry::builtin()
}

fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        layers,
        ..Recipe::default()
    }
}

fn grain(amount: f64, size: f64, roughness: f64) -> Layer {
    Layer::new(
        GRAIN_EFFECT,
        json!({"amount": amount, "size": size, "roughness": roughness}),
    )
}

/// The reference's parameters for the slider values a layer holds, through the module's responses.
fn params(amount: f64, size: f64, roughness: f64) -> reference::GrainParams {
    let strength = |field: &str, value: f64| {
        luxforge_core::slider_response("set-grain", field).map_or(value, |r| r.strength(value))
    };
    reference::GrainParams {
        amount: strength("amount", amount),
        size: strength("size", size),
        roughness: strength("roughness", roughness),
    }
}

fn planned(
    registry: &ModuleRegistry,
    recipe: &Recipe,
    request: GpuPlanRequest,
) -> luxforge_core::GpuPlan {
    match gpu_plan(registry, recipe, request).expect("the stack compiles") {
        GpuAnswer::Plan(plan) => *plan,
        GpuAnswer::Fallback(reason) => panic!("expected a plan, got {reason}"),
    }
}

/// Whether `plan` runs the grain program.
fn runs_grain(plan: &luxforge_core::GpuPlan) -> bool {
    plan.operations().any(|operation| {
        operation
            .units
            .iter()
            .any(|unit| unit.program.entry == ENTRY)
    })
}

/// The pointwise statistics of `candidate` against `expected`, both linear, through the reference
/// quantizer, over a `width × height` frame; and how many channels are non-finite in `candidate`
/// where `expected` is finite.
fn judged(
    candidate: &[[f32; 4]],
    expected: &[[f32; 3]],
    (width, height): (u32, u32),
) -> (Statistics, usize) {
    assert_eq!(candidate.len(), expected.len());
    let non_finite = candidate
        .iter()
        .zip(expected)
        .flat_map(|(c, e)| (0..3).map(move |channel| (c[channel], e[channel])))
        .filter(|(c, e)| e.is_finite() && !c.is_finite())
        .count();
    let candidate = codes(candidate.iter().map(|texel| [texel[0], texel[1], texel[2]]));
    let expected = codes(expected.iter().copied());
    let frame = |bytes| Rgb8::new(width, height, bytes).expect("a whole frame");
    let statistics =
        preview_error::compare(frame(&candidate), frame(&expected), [0, 0, width, height])
            .expect("comparable frames");
    (statistics, non_finite)
}

fn passes(statistics: &Statistics, non_finite: usize) -> bool {
    preview_error::verdict(statistics, Class::Pointwise).passed() && non_finite == 0
}

// ---- The hash ---------------------------------------------------------------------------------

/// A probe beside the grain program's own functions, its entry `lf_grain` so every name the
/// program declares starts with it: per pixel, four block words `[op, a, b, c]`;
/// op 0 the finaliser of `a`, 1 the hash of component `a` at `(b, c)` through the program's own
/// key, 2 the lattice split of index `a` by step `b`. A `u32` answer is returned as its two 16-bit
/// halves, each exact in `f32`.
const PROBE: &str = "
fn lf_grain_film_probe_halves(value: u32) -> vec2<f32> {
    return vec2<f32>(f32(value & 0xffffu), f32(value >> 16u));
}

fn lf_grain_film_probe_key(component: u32) -> u32 {
    switch component {
        case 0u: { return lf_grain_film_key_fine; }
        case 1u: { return lf_grain_film_key_coarse; }
        default: { return lf_grain_film_key_clump; }
    }
}

fn lf_grain(rgb: vec3<f32>, pos: vec2<f32>, words: u32, block: u32) -> vec3<f32> {
    let at = block + 4u * (u32(pos.y) * lf_word(words) + u32(pos.x));
    let op = lf_block_word(at);
    let a = lf_block_word(at + 1u);
    let b = lf_block_word(at + 2u);
    let c = lf_block_word(at + 3u);
    switch op {
        case 0u: {
            return vec3<f32>(lf_grain_film_probe_halves(lf_grain_film_mix(a)), 0.0);
        }
        case 1u: {
            let row = lf_grain_film_mix(c + lf_grain_film_probe_key(a));
            let hash = lf_grain_film_mix(b + row);
            return vec3<f32>(lf_grain_film_probe_halves(hash), lf_grain_film_value(hash));
        }
        default: {
            let split = lf_grain_film_lattice(a, b);
            return vec3<f32>(lf_grain_film_probe_halves(split.x), f32(split.y));
        }
    }
}
";

/// One probed case and what the reference answers for it: `(halves, third)`.
struct Probe {
    words: [u32; 4],
    expected: [f32; 3],
}

fn halves(value: u32) -> [f32; 2] {
    [(value & 0xFFFF) as f32, (value >> 16) as f32]
}

fn mix_probe(input: u32, output: u32) -> Probe {
    let [low, high] = halves(output);
    Probe {
        words: [0, input, 0, 0],
        expected: [low, high, 0.0],
    }
}

fn hash_probe(component: u32, i: u32, j: u32, hash: u32) -> Probe {
    let [low, high] = halves(hash);
    Probe {
        words: [1, component, i, j],
        expected: [low, high, reference::lattice_value(hash) as f32],
    }
}

fn lattice_probe(index: u32, step: u32) -> Probe {
    let (cell, fraction) = reference::lattice(index, step);
    let [low, high] = halves(cell);
    Probe {
        words: [2, index, step, 0],
        expected: [low, high, fraction as f32],
    }
}

fn word(value: &Value) -> u32 {
    u32::try_from(value.as_u64().expect("an integer")).expect("a u32")
}

/// The study's frozen vectors, then a dense sweep, on the device: the program's finaliser, keys,
/// hashes, lattice values and lattice split equal the reference's exactly.
#[test]
fn gpu_grain_hash_matches_the_frozen_vectors_exactly() {
    let test = "gpu_grain_hash_matches_the_frozen_vectors_exactly";
    let Some(qualifier) = headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let vectors: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fixtures/grain/hash-vectors.json"),
        )
        .expect("the hash vectors"),
    )
    .expect("JSON");
    let mut probes = Vec::new();
    for case in vectors["mix"].as_array().expect("mix cases") {
        probes.push(mix_probe(word(&case["input"]), word(&case["output"])));
    }
    // The keys: a hash at (0, 0) is `mix(mix(key))`, so equal hashes there need equal keys; the
    // keys' own vectors are checked through the finaliser of `SEED + component`.
    let seed = word(&vectors["seed"]);
    for (component, key) in vectors["component_keys"]
        .as_array()
        .expect("keys")
        .iter()
        .enumerate()
    {
        probes.push(mix_probe(seed.wrapping_add(component as u32), word(key)));
    }
    let frozen = vectors["hash"].as_array().expect("hash cases");
    for case in frozen {
        probes.push(hash_probe(
            word(&case["component"]),
            word(&case["i"]),
            word(&case["j"]),
            word(&case["hash"]),
        ));
    }
    let named = probes.len();
    // A dense sweep: every component at positions over the whole u32 range, and the lattice split
    // at every step the field uses near positions that cross 2^16.
    let mut state = 0x6772_6169_u32;
    let mut next = || {
        state = reference::mix(state.wrapping_add(0x9E37_79B9));
        state
    };
    for index in 0..8192_u32 {
        let (component, i, j) = (index % 3, next(), next());
        probes.push(hash_probe(
            component,
            i,
            j,
            reference::hash(component, i, j),
        ));
        let x = next();
        probes.push(mix_probe(x, reference::mix(x)));
    }
    for step in [1_u32, 2731, 10_468, 10_923, 20_937, 41_874, 65_535, 65_536] {
        for index in [
            0_u32,
            1,
            65_535,
            65_536,
            65_537,
            123_456,
            4_000_000,
            u32::MAX,
        ] {
            probes.push(lattice_probe(index, step));
        }
        for _ in 0..512 {
            probes.push(lattice_probe(next(), step));
        }
    }
    let width = 256_u32;
    let height = (probes.len() as u32).div_ceil(width);
    let block: Vec<u32> = (0..(width * height) as usize)
        .flat_map(|index| probes.get(index).map_or([0, 0, 0, 0], |probe| probe.words))
        .collect();
    let source = format!(
        "{}\n{PROBE}",
        GPU_PROGRAMS
            .iter()
            .find(|program| program.entry == ENTRY)
            .expect("the grain program ships")
            .source
    );
    let plan = GpuPlan {
        boundary: boundary(
            width,
            height,
            1,
            &vec![[0.25; 3]; (width * height) as usize],
        )
        .expect("a boundary"),
        texels: TexelMap::IDENTITY,
        steps: vec![GpuStep::colour(GpuProgram {
            words: vec![width],
            block: Arc::from(block),
            ..GpuProgram::new("lf_grain", source)
        })],
        region: None,
        lights: Vec::new(),
    };
    let output = qualifier.evaluate(&plan).expect("a probe readback");
    let mut missed = Vec::new();
    for (index, probe) in probes.iter().enumerate() {
        let read = [output[index][0], output[index][1], output[index][2]];
        if read.map(f32::to_bits) != probe.expected.map(f32::to_bits) {
            missed.push(format!(
                "{:?}: read {read:?}, expected {:?}",
                probe.words, probe.expected
            ));
        }
    }
    eprintln!(
        "{test}: {} probes ({named} frozen vectors, {} swept), {} differ",
        probes.len(),
        probes.len() - named,
        missed.len()
    );
    assert!(
        missed.is_empty(),
        "{test}: {:?}",
        &missed[..missed.len().min(8)]
    );
}

// ---- Full resolution --------------------------------------------------------------------------

/// The CPU units `layer` compiles to over `stage` at full resolution, as the host compiles them.
fn cpu_units(layer: &Layer, stage: Stage) -> Vec<Arc<dyn PointwiseColor>> {
    let registry = registry();
    let (module, _) = registry
        .effect(&layer.effect_id)
        .expect("a built-in effect");
    match module
        .compile(
            &layer.effect_id,
            layer.effect_format,
            &layer.payload,
            CompileStage::exact(stage),
        )
        .expect("the layer compiles")
    {
        Processing::Color(operation) => operation.units().to_vec(),
        other => panic!("expected a colour operation, got {other:?}"),
    }
}

/// The grain at the slider corners, the defaults and a small amount, over the dense grid at full
/// resolution near the origin and over a window of a large stage whose positions cross `2^16`:
/// within the pointwise limits of the `f64` reference and of the CPU unit, nothing non-finite.
#[test]
#[slow]
fn gpu_grain_meets_the_pointwise_limits_at_full_resolution() {
    let test = "gpu_grain_meets_the_pointwise_limits_at_full_resolution";
    let Some(qualifier) = headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let registry = registry();
    let (width, height) = (256_u32, 256_u32);
    let mut settings = Vec::new();
    for size in [0.0, 25.0, 50.0, 100.0] {
        for roughness in [0.0, 50.0, 100.0] {
            settings.push((100.0, size, roughness));
        }
    }
    settings.extend([(50.0, 25.0, 50.0), (1.0, 25.0, 50.0), (37.0, 63.0, 81.0)]);
    // Near the origin, and a window of a 140 000-pixel square stage crossing 2^16 on both axes.
    let placements = [
        ("origin", Stage { width, height }, (0_u32, 0_u32)),
        (
            "past 2^16",
            Stage {
                width: 140_000,
                height: 140_000,
            },
            (65_536 - 100, 65_536 - 157),
        ),
    ];
    let mut missed = Vec::new();
    for (index, &(amount, size, roughness)) in settings.iter().enumerate() {
        let layer = grain(amount, size, roughness);
        let p = params(amount, size, roughness);
        for (place, stage, (ox, oy)) in placements {
            let name = format!("amount {amount} size {size} roughness {roughness} at {place}");
            let inputs = grid(width, height, 0x6772_0000 + index as u64);
            let plan = planned(
                &registry,
                &recipe(vec![layer.clone()]),
                GpuPlanRequest::exact(0, stage),
            );
            assert!(runs_grain(&plan), "{name}: the plan runs the program");
            let held = boundary(width, height, 1, &inputs).expect("a boundary");
            let converted = surface_plan_at(&plan, held, (ox, oy), None).expect("a runnable plan");
            let gpu = qualifier.evaluate(&converted).expect("a readback");
            // The reference, in f64, and the CPU unit, in f32, at the same stage pixels.
            let reference: Vec<[f32; 3]> = inputs
                .iter()
                .enumerate()
                .map(|(at, rgb)| {
                    let (x, y) = (at as u32 % width + ox, at as u32 / width + oy);
                    reference::grain_pixel(x, y, rgb.map(f64::from), &p, reference::Footprint::FULL)
                        .map(|value| value as f32)
                })
                .collect();
            let units = cpu_units(&layer, stage);
            let mut cpu = inputs.clone();
            for (y, row) in cpu.chunks_mut(width as usize).enumerate() {
                for unit in &units {
                    unit.apply_row(y as u32 + oy, ox, row);
                }
            }
            let (against_reference, non_finite) = judged(&gpu, &reference, (width, height));
            let (against_cpu, _) = judged(&gpu, &cpu, (width, height));
            let mut linear = 0.0_f64;
            for (g, r) in gpu.iter().zip(&reference) {
                for channel in 0..3 {
                    if (-1.0..=2.0).contains(&r[channel]) && g[channel].is_finite() {
                        let difference = f64::from((g[channel] - r[channel]).abs())
                            / f64::from(r[channel].abs().max(1.0));
                        linear = linear.max(difference);
                    }
                }
            }
            let passed = passes(&against_reference, non_finite)
                && preview_error::verdict(&against_cpu, Class::Pointwise).passed();
            eprintln!(
                "{ENTRY} {name}: reference {} | CPU unit {} | non-finite {non_finite} | linear \
                 rel {linear:.2e}{}",
                figures(&against_reference),
                figures(&against_cpu),
                if passed { "" } else { " MISS" }
            );
            if !passed {
                missed.push(name);
            }
        }
    }
    assert!(missed.is_empty(), "{ENTRY} misses on {missed:?}");
}

/// A drafted Amount 0 layer compiles in its GPU shape to the program, which returns every value of
/// the grid, extremes included, bit for bit.
#[test]
fn gpu_grain_a_neutral_draft_is_the_exact_identity() {
    let test = "gpu_grain_a_neutral_draft_is_the_exact_identity";
    let Some(qualifier) = headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let registry = registry();
    let (width, height) = (128_u32, 128_u32);
    for payload in [
        json!({}),
        json!({"amount": 0, "size": 100, "roughness": 100}),
    ] {
        let layer = Layer::new(GRAIN_EFFECT, payload.clone());
        let plan = planned(
            &registry,
            &recipe(vec![layer]),
            GpuPlanRequest::exact(0, Stage { width, height }).drafted(0),
        );
        assert!(runs_grain(&plan), "{payload}: the GPU shape holds the unit");
        let inputs = grid(width, height, 0x6e65_7574);
        let held = boundary(width, height, 1, &inputs).expect("a boundary");
        let output = qualifier
            .evaluate(&surface_plan(&plan, held).expect("a runnable plan"))
            .expect("a readback");
        for (index, (read, input)) in output.iter().zip(&inputs).enumerate() {
            assert_eq!(
                [read[0], read[1], read[2]].map(f32::to_bits),
                input.map(f32::to_bits),
                "{payload}: pixel {index}"
            );
        }
    }
}

// ---- Windows and tiles ------------------------------------------------------------------------

/// The tiles' side: twelve of them over the output stage, the last column and row narrower.
const SIDE: u32 = 96;

/// Under a straightened crop, on both paths: the whole output stage drawn at once meets the
/// pointwise limits against the reference renderer's frame, and a 100% view's window and every
/// tile of the picture at rest, each over the window of the source its rectangle reads, draw the
/// whole frame's codes and values there bit for bit.
#[test]
#[slow]
fn gpu_grain_windows_and_tiles_draw_the_whole_frame() {
    let test = "gpu_grain_windows_and_tiles_draw_the_whole_frame";
    let Some(qualifier) = headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    let registry = registry();
    let stage = Stage {
        width: WIDTH,
        height: HEIGHT,
    };
    let crop = {
        let crop_stage = luxforge_core::CropStage {
            width: WIDTH,
            height: HEIGHT,
            angle: 7.0,
        };
        let (box_width, box_height) = crop_stage.bounding_box();
        let fitted = crop_stage.fit_about_center(luxforge_core::BoxRect {
            x: 0.08 * box_width,
            y: 0.1 * box_height,
            width: 0.85 * box_width,
            height: 0.8 * box_height,
        });
        Layer::crop(fitted.normalized(&crop_stage))
    };
    for format in [BoundaryFormat::Half, BoundaryFormat::Float] {
        for (amount, size, roughness) in
            [(100.0, 0.0, 0.0), (80.0, 40.0, 70.0), (100.0, 100.0, 100.0)]
        {
            let name = format!("{format:?} amount {amount} size {size} roughness {roughness}");
            let recipe = recipe(vec![
                Layer::new(BASIC_EFFECT, json!({"exposure": 0.4, "contrast": 20.0})),
                crop.clone(),
                grain(amount, size, roughness),
            ]);
            let source = source(format);
            let pixels = whole(&source);
            let request = GpuPlanRequest::exact(0, stage);
            let request = match format {
                BoundaryFormat::Float => request.linear(),
                BoundaryFormat::Half => request,
            };
            let plan = planned(&registry, &recipe, request);
            assert!(runs_grain(&plan), "{name}: the plan runs the program");
            assert_eq!(
                plan.output.len(),
                1,
                "{name}: grain is an output operation after the tail"
            );
            let output = plan.geometry.output();
            let context = RenderContext::new();
            let exact = render(
                &registry,
                source.whole().expect("a test's source is never paged"),
                &recipe,
                RenderOptions::exact(&Cancel::never()),
                &context,
            )
            .expect("the exact render");
            let draw = |window: Region, rect: Option<Region>| {
                let held = boundary_as(
                    format,
                    window.width,
                    window.height,
                    1,
                    &cut(&pixels, window),
                )
                .expect("a boundary");
                let converted = surface_plan_over(&plan, held, (window.x0, window.y0), None, rect)
                    .expect("a runnable plan");
                let codes = qualifier.evaluate_codes(&converted).expect("the codes");
                let values: Vec<[f32; 4]> = qualifier.evaluate(&converted).expect("the values");
                (codes, values)
            };
            let (whole_codes, whole_values) = draw(
                Region {
                    x0: 0,
                    y0: 0,
                    width: WIDTH,
                    height: HEIGHT,
                },
                None,
            );
            assert_eq!(
                whole_codes.len(),
                (output.width * output.height) as usize,
                "{name}"
            );
            // The whole frame against the reference renderer's.
            let frame = exact.frame(SnapshotId::new()).expect("the reference frame");
            let reference: Vec<u8> = frame
                .rgba
                .chunks_exact(4)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect();
            let drawn: Vec<u8> = whole_codes
                .iter()
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2]])
                .collect();
            let statistics = preview_error::compare(
                Rgb8::new(output.width, output.height, &drawn).expect("a frame"),
                Rgb8::new(output.width, output.height, &reference).expect("a frame"),
                [0, 0, output.width, output.height],
            )
            .expect("comparable frames");
            eprintln!(
                "{test}: {name}: whole frame against the reference {}",
                figures(&statistics)
            );
            assert!(
                preview_error::verdict(&statistics, Class::Pointwise).passed(),
                "{name}: the whole frame misses the pointwise limits"
            );
            // A 100% view's window, then the picture at rest's tiles: each its rectangle of the
            // output stage over the window of the source it reads, anchored to the plan.
            let mut rects = vec![Region {
                x0: 37,
                y0: 23,
                width: 151,
                height: 101,
            }];
            for y0 in (0..output.height).step_by(SIDE as usize) {
                for x0 in (0..output.width).step_by(SIDE as usize) {
                    rects.push(Region {
                        x0,
                        y0,
                        width: SIDE.min(output.width - x0),
                        height: SIDE.min(output.height - y0),
                    });
                }
            }
            for rect in &rects {
                let held = luxforge_core::qualification::region_boundary(
                    &exact,
                    0,
                    [rect.x0, rect.y0, rect.width, rect.height],
                    format,
                )
                .expect("the rectangle's window");
                let window = anchored(
                    Region {
                        x0: held.origin.0,
                        y0: held.origin.1,
                        width: held.width,
                        height: held.height,
                    },
                    plan.anchor(),
                );
                let (codes, values) = draw(window, Some(*rect));
                assert_eq!(codes.len(), rect.pixels() as usize, "{name}: {rect:?}");
                for row in 0..rect.height {
                    for column in 0..rect.width {
                        let at = (row * rect.width + column) as usize;
                        let whole_at = ((rect.y0 + row) * output.width + rect.x0 + column) as usize;
                        assert_eq!(
                            codes[at],
                            whole_codes[whole_at],
                            "{name}: {rect:?} over {window:?}, pixel ({}, {})",
                            rect.x0 + column,
                            rect.y0 + row
                        );
                        assert_eq!(
                            values[at].map(f32::to_bits),
                            whole_values[whole_at].map(f32::to_bits),
                            "{name}: {rect:?} over {window:?}, pixel ({}, {})",
                            rect.x0 + column,
                            rect.y0 + row
                        );
                    }
                }
            }
            eprintln!(
                "{test}: {name}: a window and {} tiles, each the whole frame's bit for bit",
                rects.len() - 1
            );
        }
    }
}

// ---- Reduced stages ---------------------------------------------------------------------------

fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let angle = -std::f64::consts::TAU / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (s, c) = (angle * k as f64).sin_cos();
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * c - im[b] * s;
                let ti = re[b] * s + im[b] * c;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

const BANDS: usize = 8;

/// The share of an `n × n` image's variance in each of eight equal radial bands of its periodogram
/// over `[0, 0.5)` cycles a pixel, as the study measures it.
fn band_shares(image: &[f64], n: usize) -> [f64; BANDS] {
    let mean = image.iter().sum::<f64>() / image.len() as f64;
    let mut re: Vec<f64> = image.iter().map(|v| v - mean).collect();
    let mut im = vec![0.0; n * n];
    for row in 0..n {
        fft(
            &mut re[row * n..(row + 1) * n],
            &mut im[row * n..(row + 1) * n],
        );
    }
    let (mut column_re, mut column_im) = (vec![0.0; n], vec![0.0; n]);
    for column in 0..n {
        for row in 0..n {
            column_re[row] = re[row * n + column];
            column_im[row] = im[row * n + column];
        }
        fft(&mut column_re, &mut column_im);
        for row in 0..n {
            re[row * n + column] = column_re[row];
            im[row * n + column] = column_im[row];
        }
    }
    let signed = |k: usize| {
        (if k > n / 2 {
            k as f64 - n as f64
        } else {
            k as f64
        }) / n as f64
    };
    let mut sums = [0.0; BANDS];
    for ky in 0..n {
        for kx in 0..n {
            let radius = signed(kx).hypot(signed(ky));
            if (kx == 0 && ky == 0) || radius >= 0.5 {
                continue;
            }
            sums[(radius * 2.0 * BANDS as f64) as usize] +=
                re[ky * n + kx].powi(2) + im[ky * n + kx].powi(2);
        }
    }
    let total: f64 = sums.iter().sum();
    sums.map(|sum| sum / total)
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn deviation(values: &[f64]) -> f64 {
    let m = mean(values);
    (values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

/// A grain stack planned at Fit's reduced stage of `n × n` pixels for a full stage of `full ×
/// full`, as the proxy phase plans a drag's frame, over flat encoded greys 0.2, 0.5 and 0.8 at
/// Amount 100: on the device within the pointwise limits of the reference at that footprint, and,
/// for an integer footprint, the encoded values' standard deviation within 5%, each radial band's
/// share of the variance within 0.08 and the mean within 0.5 code of the full-resolution grain
/// reduced by its linear-light area average (`docs/design/grain-study.md`, "Frozen tolerances").
#[test]
#[slow]
fn gpu_grain_a_reduced_stage_draws_the_studys_reduced_field() {
    let test = "gpu_grain_a_reduced_stage_draws_the_studys_reduced_field";
    let Some(qualifier) = headless(test) else {
        return;
    };
    eprintln!("{test}: adapter {}", qualifier.adapter());
    const N: u32 = 256;
    const TONES: [f64; 3] = [0.2, 0.5, 0.8];
    let registry = registry();
    let mut missed = Vec::new();
    for full in [2 * N, 4 * N, 8 * N, 1000] {
        let integer = full % N == 0;
        let s = full / N;
        for (size, roughness) in [(0.0, 0.0), (25.0, 50.0), (100.0, 100.0)] {
            let name = format!("full {full} for {N} size {size} roughness {roughness}");
            let p = params(100.0, size, roughness);
            let plan = planned(
                &registry,
                &recipe(vec![grain(100.0, size, roughness)]),
                GpuPlanRequest::fit(
                    0,
                    Stage {
                        width: N,
                        height: N,
                    },
                    Stage {
                        width: full,
                        height: full,
                    },
                ),
            );
            assert!(runs_grain(&plan), "{name}: the plan runs the program");
            let footprint = reference::Footprint::of((N, N), (full, full));
            // Each grey as the byte path's half-float boundary holds it.
            let greys: Vec<f64> = TONES
                .iter()
                .map(|&e| f64::from(held(srgb::decode_encoded(e) as f32)))
                .collect();
            // The full-resolution grain over each stage pixel's area, reduced in linear light.
            let target: Vec<Vec<f64>> = if integer {
                let field = reference::Field::new(p.size, p.roughness, reference::Footprint::FULL);
                let beta = reference::beta(p.amount);
                let block = f64::from(s * s);
                let mut target = vec![Vec::with_capacity((N * N) as usize); TONES.len()];
                for y in 0..N {
                    for x in 0..N {
                        let mut sums = [0.0; 3];
                        for dy in 0..s {
                            for dx in 0..s {
                                let g = field.at(x * s + dx, y * s + dy);
                                for (sum, &grey) in sums.iter_mut().zip(&greys) {
                                    *sum += reference::apply([grey; 3], g, beta, field.variance)[1];
                                }
                            }
                        }
                        for tone in 0..TONES.len() {
                            target[tone].push(srgb::encode_extended(sums[tone] / block));
                        }
                    }
                }
                target
            } else {
                Vec::new()
            };
            for (tone, (&encoded, &grey)) in TONES.iter().zip(&greys).enumerate() {
                let label = format!("{name} tone {encoded}");
                let inputs = vec![[grey as f32; 3]; (N * N) as usize];
                let input = boundary(N, N, 1, &inputs).expect("a boundary");
                let gpu = qualifier
                    .evaluate(&surface_plan(&plan, input).expect("a runnable plan"))
                    .expect("a readback");
                let reference: Vec<[f32; 3]> = (0..N * N)
                    .map(|at| {
                        reference::grain_pixel(at % N, at / N, [grey; 3], &p, footprint)
                            .map(|value| value as f32)
                    })
                    .collect();
                let (statistics, non_finite) = judged(&gpu, &reference, (N, N));
                let mut passed = passes(&statistics, non_finite);
                let mut line = format!("reference {}", figures(&statistics));
                if integer {
                    let candidate: Vec<f64> = gpu
                        .iter()
                        .map(|texel| srgb::encode_extended(f64::from(texel[1])))
                        .collect();
                    let target = &target[tone];
                    let ratio = deviation(&candidate) / deviation(target);
                    let band = band_shares(target, N as usize)
                        .iter()
                        .zip(band_shares(&candidate, N as usize))
                        .map(|(t, c)| (t - c).abs())
                        .fold(0.0, f64::max);
                    let codes = (mean(&candidate) - mean(target)) * 255.0;
                    passed &= (ratio - 1.0).abs() <= 0.05 && band <= 0.08 && codes.abs() <= 0.5;
                    line.push_str(&format!(
                        " | against the reduced full grain: sd ratio {ratio:.4}, worst band \
                         {band:.4}, mean {codes:+.3} codes"
                    ));
                }
                eprintln!(
                    "{ENTRY} {label}: {line}{}",
                    if passed { "" } else { " MISS" }
                );
                if !passed {
                    missed.push(label);
                }
            }
        }
    }
    assert!(
        missed.is_empty(),
        "{ENTRY} misses at reduced stages on {missed:?}"
    );
}
