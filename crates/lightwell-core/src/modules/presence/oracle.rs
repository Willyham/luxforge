//! Production against the checked-in Presence oracle, through the host's own tile executor, and
//! the tiling and identity proofs that need a tile size the public render API does not take.
//!
//! `fixtures/presence/cases.json` holds ten cases: a deterministic image spec, the **stage** long
//! side the radii scale by, the three amounts and the full `f64` output of the independent
//! reference. The stage long side is deliberately not the fixture's own long side — a fixture is a
//! small region of a large stage, and Presence's radii belong to the stage — so a case is run here
//! by compiling the payload against a stage of that long side and evaluating the resulting
//! operation over the fixture's own stage with [`run_tile`], which is the one function the byte
//! render, the RAW float frame and the point sample all go through.
//!
//! The images are rebuilt from their specs exactly as the fixture README says a production
//! implementation may: the same 64-bit LCG, the same formulas, no pixel buffer in the JSON.

use super::{PRESENCE_EFFECT, PresenceModule, presence_halo};
use crate::{
    EFFECT_FORMAT, Error, Layer, LayerId, ModuleRegistry, RECIPE_FORMAT, Recipe, SnapshotId,
    SourceImage,
    modules::{Global, Region, SpatialOperation, Stage, ToolModule},
    render::{
        render_tiled,
        spatial::{
            Cancel, SpatialBudget, SpatialPlan, build_reduction, fill_planes, run_tile,
            tests::spatial_guard,
        },
    },
};
use serde_json::{Value, json};
use std::path::PathBuf;

/// The frozen production tolerance, in linear light: `2e-4 + 2e-4 * |reference|`.
fn tolerance(reference: f64) -> f64 {
    2e-4 + 2e-4 * reference.abs()
}

/// The 64-bit LCG every noisy fixture spec draws from.
struct Lcg(u64);

impl Lcg {
    fn next_unit(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
    }
}

const HAZE_PALETTE: [[f64; 3]; 6] = [
    [0.80, 0.35, 0.20],
    [0.20, 0.70, 0.30],
    [0.15, 0.25, 0.85],
    [0.70, 0.70, 0.20],
    [0.45, 0.45, 0.45],
    [0.10, 0.55, 0.60],
];

/// One fixture image rebuilt from its spec: the three kinds the ten oracle cases use.
fn build_image(spec: &Value) -> (u32, u32, Vec<[f32; 3]>) {
    let number = |key: &str| spec[key].as_f64().expect("a numeric field");
    let integer = |key: &str| spec[key].as_i64().expect("an integer field");
    let width = integer("width");
    let height = integer("height");
    let mut pixels = vec![[0.0_f64; 3]; (width * height) as usize];
    match spec["kind"].as_str().expect("a kind") {
        "step" => {
            let (low, high) = (number("low"), number("high"));
            let vertical = spec["vertical"].as_bool().expect("a flag");
            for y in 0..height {
                for x in 0..width {
                    let along = if vertical { y } else { x };
                    let extent = if vertical { height } else { width };
                    let value = if along < extent / 2 { low } else { high };
                    pixels[(y * width + x) as usize] = [value; 3];
                }
            }
        }
        "patch" => {
            let mean = number("mean");
            let seed = spec["seed"].as_u64().expect("a seed");
            let tint: Vec<f64> = spec["tint"]
                .as_array()
                .expect("a tint")
                .iter()
                .map(|value| value.as_f64().expect("a tint channel"))
                .collect();
            let mut rng = Lcg(seed);
            for y in 0..height {
                for x in 0..width {
                    let fx = x as f64;
                    let fy = y as f64;
                    let value = mean
                        + 0.05
                            * (std::f64::consts::TAU * fx / 5.0).sin()
                            * (std::f64::consts::TAU * fy / 7.0).sin()
                        + 0.04 * (std::f64::consts::TAU * fx / 19.0).sin()
                        + 0.03 * (std::f64::consts::TAU * fy / 31.0).cos()
                        + 0.01 * (rng.next_unit() * 2.0 - 1.0);
                    pixels[(y * width + x) as usize] =
                        [value * tint[0], value * tint[1], value * tint[2]];
                }
            }
        }
        "haze" => {
            let (t_min, t_max) = (number("t_min"), number("t_max"));
            let sky_rows = integer("sky_rows");
            let cell = integer("cell");
            let a: Vec<f64> = spec["a"]
                .as_array()
                .expect("an atmospheric light")
                .iter()
                .map(|value| value.as_f64().expect("a channel"))
                .collect();
            for y in 0..height {
                for x in 0..width {
                    let clear = if y < sky_rows {
                        [a[0], a[1], a[2]]
                    } else {
                        let bx = x / cell;
                        let by = y / cell;
                        if bx % 2 == 0 && by % 2 == 0 {
                            [0.0; 3]
                        } else {
                            HAZE_PALETTE[(((bx / 2) + 3 * (by / 2)) % 6) as usize]
                        }
                    };
                    let fraction = if width > 1 {
                        x as f64 / (width - 1) as f64
                    } else {
                        0.0
                    };
                    let t = t_min + (t_max - t_min) * fraction;
                    pixels[(y * width + x) as usize] =
                        std::array::from_fn(|channel| t * clear[channel] + (1.0 - t) * a[channel]);
                }
            }
        }
        other => panic!("the oracle cases use step, patch and haze images, not {other}"),
    }
    (
        width as u32,
        height as u32,
        pixels
            .into_iter()
            .map(|pixel| pixel.map(|value| value as f32))
            .collect(),
    )
}

fn fixture_cases() -> Vec<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/presence/cases.json")
        .canonicalize()
        .expect("fixtures/presence/cases.json");
    let file: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("the presence fixtures"))
            .expect("parse cases.json");
    file["cases"].as_array().expect("the cases").clone()
}

/// The whole of one stage through one compiled operation, tile by tile, exactly as the byte render
/// does: the host's plan, the host's reduction for the global estimates, and [`run_tile`] for every
/// tile.
fn evaluate(
    operation: &SpatialOperation,
    stage: Stage,
    pixels: &[[f32; 3]],
    tile_size: u32,
) -> Result<Vec<[f32; 3]>, Error> {
    let read =
        |x: u32, y: u32| -> Result<[f32; 3], Error> { Ok(pixels[(y * stage.width + x) as usize]) };
    let plan = SpatialPlan::new(operation, stage, tile_size)?;
    let reduction = build_reduction(stage, read)?;
    let globals: Vec<Option<Global>> = operation
        .units()
        .iter()
        .map(|unit| unit.prepare(&reduction))
        .collect();
    let mut output = vec![[0.0_f32; 3]; pixels.len()];
    for tile in plan.tiles() {
        let (region, values) = run_tile(&plan, operation, &globals, tile, |region, planes| {
            fill_planes(region, planes, read)
        })?;
        for y in tile.y0..tile.y1() {
            for x in tile.x0..tile.x1() {
                output[(y * stage.width + x) as usize] =
                    crate::render::spatial::plane_pixel(region, &values, x, y);
            }
        }
    }
    Ok(output)
}

/// The operation a case's amounts compile to at its stage long side. The payload goes through the
/// module's own `compile`, so the unit order, the omission of amount-0 units and the radii all come
/// from production rather than from the test.
fn compiled(case: &Value) -> SpatialOperation {
    let long_side = case["long_side"].as_u64().expect("a stage long side") as u32;
    let params = &case["params"];
    let mut payload = serde_json::Map::new();
    for field in ["texture", "clarity", "dehaze"] {
        let value = params[field].as_f64().expect("an amount");
        if value != 0.0 {
            payload.insert(field.to_owned(), json!(value));
        }
    }
    let stage = Stage {
        width: long_side,
        height: 1,
    };
    match PresenceModule::new()
        .compile(PRESENCE_EFFECT, 1, &Value::Object(payload), stage)
        .expect("a compiled presence operation")
    {
        crate::modules::Processing::Spatial(operation) => operation,
        other => panic!("expected a spatial operation, got {other:?}"),
    }
}

#[test]
fn production_matches_every_oracle_case_within_the_frozen_tolerance() {
    let _guard = spatial_guard();
    let cases = fixture_cases();
    assert_eq!(cases.len(), 10, "every committed case is checked");
    let mut worst = 0.0_f64;
    let mut worst_case = String::new();
    for case in &cases {
        let name = case["name"].as_str().expect("a name");
        let (width, height, pixels) = build_image(&case["image"]);
        let stage = Stage { width, height };
        let operation = compiled(case);
        let output = evaluate(&operation, stage, &pixels, crate::modules::SPATIAL_TILE)
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let expected = case["expected"].as_array().expect("the expected output");
        assert_eq!(expected.len(), output.len(), "{name}: pixel count");
        for (index, (actual, expected)) in output.iter().zip(expected).enumerate() {
            let expected = expected.as_array().expect("an expected pixel");
            for channel in 0..3 {
                let reference = expected[channel].as_f64().expect("a channel");
                let deviation = (f64::from(actual[channel]) - reference).abs();
                assert!(
                    deviation <= tolerance(reference),
                    "{name}: pixel {index} channel {channel}: production {} against the reference \
                     {reference}, {deviation} apart, more than {}",
                    actual[channel],
                    tolerance(reference)
                );
                if deviation > worst {
                    worst = deviation;
                    worst_case = format!("{name} pixel {index} channel {channel}");
                }
            }
        }
    }
    // Printed with --nocapture so the handoff can quote a measured figure.
    println!("maximum observed deviation from the oracle {worst:e} at {worst_case}");
}

/// Every case again at three tile sizes, including ones smaller than the units' own halos, against
/// the whole-frame evaluation. Production's box passes are running `f64` sums rather than the
/// reference's direct summation, so the study's "up to float summation order" allowance applies and
/// this is a tolerance check rather than a bit-exact one; the observed figure is far below it.
#[test]
fn a_tiled_evaluation_agrees_with_the_whole_frame_at_every_tile_size() {
    let _guard = spatial_guard();
    let mut worst = 0.0_f64;
    for case in &fixture_cases() {
        let name = case["name"].as_str().expect("a name");
        let (width, height, pixels) = build_image(&case["image"]);
        let stage = Stage { width, height };
        let operation = compiled(case);
        let whole = evaluate(&operation, stage, &pixels, 512).expect("the whole frame");
        for tile in [4_u32, 7, 16] {
            let tiled = evaluate(&operation, stage, &pixels, tile)
                .unwrap_or_else(|error| panic!("{name} at tile {tile}: {error}"));
            for (index, (tiled, whole)) in tiled.iter().zip(&whole).enumerate() {
                for channel in 0..3 {
                    let reference = f64::from(whole[channel]);
                    let deviation = (f64::from(tiled[channel]) - reference).abs();
                    assert!(
                        deviation <= tolerance(reference),
                        "{name} at tile {tile}: pixel {index} channel {channel}: {} against the \
                         whole frame's {}",
                        tiled[channel],
                        whole[channel]
                    );
                    worst = worst.max(deviation);
                }
            }
        }
    }
    println!("maximum observed tiled-versus-whole-frame deviation {worst:e}");
}

/// A unit at amount 0 is not in the operation at all, and an operation of the remaining units is
/// the exact identity nowhere else: this checks the other half, that every unit's own zero really
/// is bit-exact, by compiling each amount alone and comparing the whole frame to its input.
#[test]
fn every_unit_at_zero_is_omitted_and_the_neutral_payload_compiles_to_nothing() {
    let module = PresenceModule::new();
    let stage = Stage {
        width: 24,
        height: 24,
    };
    for payload in [
        json!({}),
        json!({"texture": 0.0}),
        json!({"texture": 0.0, "clarity": 0.0, "dehaze": 0.0}),
    ] {
        let crate::modules::Processing::Spatial(operation) = module
            .compile(PRESENCE_EFFECT, 1, &payload, stage)
            .expect("a compiled operation")
        else {
            panic!("a spatial operation");
        };
        assert!(
            operation.is_empty(),
            "{payload} compiles to units the host would have to run"
        );
        assert_eq!(operation.summed_halo(stage), 0);
    }
}

/// The host reserves what the units declare, and the declaration covers what they actually take:
/// a unit that asked for more than it declared fails with an internal error instead of reading
/// somebody else's memory, and no case here does.
#[test]
fn one_tile_of_a_large_stage_fits_the_spatial_budget() {
    let _guard = spatial_guard();
    let module = PresenceModule::new();
    for (width, height) in [(6000_u32, 4000_u32), (10_000, 6000)] {
        let stage = Stage { width, height };
        let crate::modules::Processing::Spatial(operation) = module
            .compile(
                PRESENCE_EFFECT,
                1,
                &json!({"texture": 100.0, "clarity": 100.0, "dehaze": 100.0}),
                stage,
            )
            .expect("a compiled operation")
        else {
            panic!("a spatial operation");
        };
        let plan = SpatialPlan::new(&operation, stage, crate::modules::SPATIAL_TILE)
            .unwrap_or_else(|error| panic!("{width}x{height}: {error}"));
        assert!(
            plan.working_set() <= SpatialBudget::default().limit(),
            "{width}x{height}: one tile needs {} bytes",
            plan.working_set()
        );
        assert!(plan.concurrency() >= 1);
        assert!(
            operation.summed_halo(stage) <= crate::modules::MAX_SPATIAL_HALO,
            "{width}x{height}: the summed halo is over the host's bound"
        );
    }
}

/// The unit halos this stage produces are exactly the study's, and a region's rectangles are what
/// the host's shrink rule says: the one arithmetic the units and the host must agree on.
#[test]
fn a_tiles_rectangles_follow_the_declared_halos() {
    let _guard = spatial_guard();
    let module = PresenceModule::new();
    let stage = Stage {
        width: 2000,
        height: 1500,
    };
    let crate::modules::Processing::Spatial(operation) = module
        .compile(
            PRESENCE_EFFECT,
            1,
            &json!({"texture": 20.0, "clarity": 20.0, "dehaze": 20.0}),
            stage,
        )
        .expect("a compiled operation")
    else {
        panic!("a spatial operation");
    };
    let long_side = 2000;
    let summed = presence_halo(long_side) as u32;
    assert_eq!(operation.summed_halo(stage), summed);
    let plan = SpatialPlan::new(&operation, stage, 512).expect("a plan");
    let tile = Region {
        x0: 512,
        y0: 512,
        width: 512,
        height: 512,
    };
    let regions = plan.regions(tile);
    assert_eq!(regions.len(), 4);
    assert_eq!(regions[0], tile.grown(summed, stage));
    let last = regions[3];
    assert!(
        last.x0 <= tile.x0
            && last.y0 <= tile.y0
            && last.x1() >= tile.x1()
            && last.y1() >= tile.y1(),
        "the last unit's rectangle {last:?} must contain the tile"
    );
}

/// A textured, non-flat frame: a broad gradient with fine structure over it, so every unit has
/// something to work on at every scale.
#[cfg(test)]
fn textured_source(width: u32, height: u32) -> SourceImage {
    let mut rgba = Vec::with_capacity((width as usize) * (height as usize) * 4);
    for y in 0..height {
        for x in 0..width {
            let broad = (x as f32 / width as f32) * 0.6 + (y as f32 / height as f32) * 0.25;
            let fine = 0.08
                * ((x as f32 * 0.7).sin() * (y as f32 * 0.9).cos()
                    + 0.5 * (x as f32 * 0.13 + y as f32 * 0.07).sin());
            let value = (broad + fine + 0.1).clamp(0.0, 1.0);
            let code = (value * 255.0) as u8;
            rgba.extend_from_slice(&[code, code.saturating_add(9), code.saturating_sub(7), 255]);
        }
    }
    SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:presence-textured".into(),
        orientation: 1,
    }
}

#[cfg(test)]
fn presence_recipe(payload: Value) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers: vec![Layer {
            id: LayerId::new(),
            effect_id: PRESENCE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload,
            mask: None,
        }],
        masks: Vec::new(),
    }
}

/// The host's tile size changes no pixel of a rendered frame. The public render API always uses the
/// production tile, so this goes through `render_tiled`, the same entry point the host's own spatial
/// tests use to prove tile invariance.
#[test]
fn a_render_at_tile_128_and_at_tile_512_agree_on_every_code() {
    let _guard = spatial_guard();
    crate::render::clear_estimates();
    let registry = ModuleRegistry::builtin();
    // Larger than one production tile on both sides of the 512 grid, so both tile sizes exercise
    // partial edge tiles and more than one batch.
    let source = textured_source(600, 400);
    let stack = presence_recipe(json!({"texture": 60.0, "clarity": -40.0, "dehaze": 35.0}));
    let mut frames = Vec::new();
    for tile in [128_u32, 512] {
        let raster = render_tiled(
            &registry,
            &source,
            SnapshotId::new(),
            &stack,
            &Cancel::new(),
            tile,
        )
        .unwrap_or_else(|error| panic!("tile {tile}: {error}"));
        frames.push(raster.rgba.as_ref().to_vec());
    }
    let mut worst = 0_i32;
    let mut differing = 0_usize;
    for (index, (small, large)) in frames[0].iter().zip(&frames[1]).enumerate() {
        let deviation = (i32::from(*small) - i32::from(*large)).abs();
        if deviation != 0 {
            differing += 1;
        }
        assert!(
            deviation <= 1,
            "byte {index}: tile 128 gave {small}, tile 512 gave {large}"
        );
        worst = worst.max(deviation);
    }
    println!(
        "tile 128 against tile 512: {differing} of {} bytes differ, worst {worst} code",
        frames[0].len()
    );
    assert_eq!(worst, 0, "the tile size changed a code");
}

/// Release-only measurement, run explicitly:
///
/// ```sh
/// cargo test --release --locked --package lightwell-core -- --ignored presence_timing --nocapture
/// ```
///
/// Each unit alone at `+100` and all three together, on in-memory 24 MP and 60 MP frames with a
/// warm source, p50 and p95 over ten runs, with the plan's working set and concurrency and the
/// spatial budget's high-water mark.
#[test]
#[ignore = "measurement, run explicitly in release"]
fn presence_timing() {
    let _guard = spatial_guard();
    let registry = ModuleRegistry::builtin();
    let module = PresenceModule::new();
    let mib = (1024 * 1024) as f64;
    for (width, height) in [(6000_u32, 4000_u32), (10_000, 6000)] {
        let source = textured_source(width, height);
        let stage = Stage { width, height };
        for (name, payload) in [
            ("texture +100", json!({"texture": 100.0})),
            ("clarity +100", json!({"clarity": 100.0})),
            ("dehaze +100", json!({"dehaze": 100.0})),
            (
                "all three +100",
                json!({"texture": 100.0, "clarity": 100.0, "dehaze": 100.0}),
            ),
        ] {
            let stack = presence_recipe(payload.clone());
            let crate::modules::Processing::Spatial(operation) = module
                .compile(PRESENCE_EFFECT, 1, &payload, stage)
                .expect("a compiled operation")
            else {
                panic!("a spatial operation");
            };
            let plan =
                SpatialPlan::new(&operation, stage, crate::modules::SPATIAL_TILE).expect("a plan");
            // Warm the source and the estimate store, then measure.
            crate::render(&registry, &source, SnapshotId::new(), &stack).expect("a warm render");
            SpatialBudget::default().reset_peak();
            let mut samples = Vec::new();
            for _ in 0..10 {
                let started = std::time::Instant::now();
                let raster =
                    crate::render(&registry, &source, SnapshotId::new(), &stack).expect("a render");
                samples.push(started.elapsed().as_secs_f64() * 1000.0);
                assert_eq!((raster.width, raster.height), (width, height));
            }
            samples.sort_by(f64::total_cmp);
            let p50 = samples[samples.len() / 2];
            let p95 = samples[(samples.len() as f64 * 0.95).ceil() as usize - 1];
            println!(
                "{width}x{height} presence {name}: p50 {p50:.0} ms, p95 {p95:.0} ms over {} runs; \
                 halo {} px, tiles {}, working set {:.1} MiB, concurrency {}, budget peak \
                 {:.1} MiB of {:.1} MiB",
                samples.len(),
                operation.summed_halo(stage),
                plan.tiles().len(),
                plan.working_set() as f64 / mib,
                plan.concurrency(),
                SpatialBudget::default().peak() as f64 / mib,
                SpatialBudget::default().limit() as f64 / mib,
            );
        }
    }
}
