use super::*;
use crate::modules::{Parallelism, Planes, PlanesMut, Region, SamplingScale, Stage, ToolModule};
use crate::render::spatial::{SpatialPlan, TileScratch, Tiling, fill_planes, run_tile};
use luxforge_reference::{SplitMix64, srgb};
use serde::Deserialize;
use serde_json::json;
use std::{f64::consts::PI, fs, path::PathBuf};

fn tiled(op: &SpatialOperation, input: &[[f32; 3]], stage: Stage, tile: u32) -> Vec<[f32; 3]> {
    if op.is_empty() {
        return input.to_vec();
    }
    let plan = SpatialPlan::new(op, stage, Tiling::Fixed(tile)).unwrap();
    // One slot for every tile, reused as a render's batch slot is.
    let mut scratch = TileScratch::for_plan(&plan);
    let mut out = vec![[0.0; 3]; input.len()];
    let globals = vec![None; op.len()];
    for tile in plan.tiles() {
        let (region, values) = run_tile(
            &plan,
            op,
            &globals,
            tile,
            Parallelism::Serial,
            &mut scratch,
            &crate::Cancel::never(),
            |region, planes| {
                fill_planes(region, planes, Parallelism::Serial, |x, y| {
                    Ok(input[(y * stage.width + x) as usize])
                })
            },
        )
        .unwrap();
        for y in tile.y0..tile.y1() {
            for x in tile.x0..tile.x1() {
                out[(y * stage.width + x) as usize] =
                    crate::render::spatial::plane_pixel(region, values, x, y);
            }
        }
    }
    out
}

#[derive(Deserialize)]
struct Spec {
    width: usize,
    height: usize,
    kind: String,
    seed: u64,
}
#[derive(Deserialize)]
struct Case {
    name: String,
    image: Spec,
    parameters: [f64; 8],
    scale: [f64; 2],
    coverage: f64,
    expected: Vec<[f64; 3]>,
}
fn gaussian(rng: &mut SplitMix64) -> f64 {
    (-2.0 * rng.next_range(f64::MIN_POSITIVE, 1.0).ln()).sqrt()
        * (2.0 * PI * rng.next_range(0.0, 1.0)).cos()
}
fn build(s: &Spec) -> Vec<[f32; 3]> {
    let mut rng = SplitMix64(s.seed);
    (0..s.width * s.height)
        .map(|i| {
            let (x, y) = (i % s.width, i / s.width);
            let rgb = match s.kind.as_str() {
                "constant" => [0.18, 0.12, 0.25],
                "impulse" => {
                    if x == s.width / 2 && y == s.height / 2 {
                        [0.8; 3]
                    } else {
                        [0.1; 3]
                    }
                }
                "grey-ramp" => {
                    [0.02 + 0.7 * x as f64 / s.width as f64 + 0.01 * gaussian(&mut rng); 3]
                }
                "saturated-ramp" => [
                    x as f64 / s.width as f64,
                    0.04,
                    1.0 - x as f64 / s.width as f64,
                ],
                "extended" => [-0.2 + x as f64 / s.width as f64 * 1.8, 0.4, 1.4],
                "grating" => [0.3 + 0.12 * (x as f64 * 2.0 * PI / 6.5).sin(); 3],
                "chroma-edge" => {
                    if x < s.width / 2 {
                        [0.6, 0.04, 0.1]
                    } else {
                        [0.05, 0.5, 0.7]
                    }
                }
                "jpeg-blocks" => [srgb::decode((32 + (x / 8 + y / 8) % 2 * 7) as u8); 3],
                "slanted-edge" => {
                    [0.1 + 0.4
                        * (0.5
                            + 0.5
                                * ((x as f64
                                    - s.width as f64 / 2.0
                                    - y as f64 * (5.0_f64.to_radians()).tan())
                                    / 0.8)
                                    .tanh()); 3]
                }
                _ => [
                    0.15 + 0.01 * gaussian(&mut rng),
                    0.2 + 0.01 * gaussian(&mut rng),
                    0.25 + 0.01 * gaussian(&mut rng),
                ],
            };
            rgb.map(|v| v as f32)
        })
        .collect()
}
fn payload(p: [f64; 8]) -> serde_json::Value {
    json!({SHARPENING:p[0],RADIUS:p[1],SHARPEN_DETAIL:p[2],SHARPEN_MASKING:p[3],LUMINANCE:p[4],LUMINANCE_DETAIL:p[5],COLOUR:p[6],COLOUR_DETAIL:p[7]})
}
fn operation(p: [f64; 8], stage: Stage, scale: [f64; 2]) -> SpatialOperation {
    let at = CompileStage {
        stage,
        full: Stage {
            width: (f64::from(stage.width) / scale[0]).round() as u32,
            height: (f64::from(stage.height) / scale[1]).round() as u32,
        },
        scale: SamplingScale {
            x: scale[0],
            y: scale[1],
        },
        gpu_shape: false,
    };
    let Processing::Spatial(op) = DetailModule::new()
        .compile(DETAIL_EFFECT, 1, &payload(p), at)
        .unwrap()
    else {
        panic!("Detail is spatial");
    };
    op
}
fn planar(pixels: &[[f32; 3]]) -> Vec<f32> {
    (0..3)
        .flat_map(|c| pixels.iter().map(move |p| p[c]))
        .collect()
}
fn pixels(planes: &[f32]) -> Vec<[f32; 3]> {
    let n = planes.len() / 3;
    (0..n)
        .map(|i| [planes[i], planes[n + i], planes[2 * n + i]])
        .collect()
}
fn run(
    op: &SpatialOperation,
    input: &[[f32; 3]],
    stage: Stage,
    parallelism: Parallelism,
) -> Vec<[f32; 3]> {
    let area = Region::whole(stage);
    let mut current = planar(input);
    for unit in op.units() {
        let mut next = vec![0.0; current.len()];
        let mut scratch = vec![0.0; (unit.scratch_bytes(stage) / 4) as usize];
        unit.apply(
            &Planes::new(stage, area, &current).unwrap(),
            &mut PlanesMut::new(stage, area, &mut next).unwrap(),
            None,
            &mut scratch,
            parallelism,
        )
        .unwrap();
        current = next;
    }
    pixels(&current)
}

#[test]
fn detail_matches_independent_f64_fixtures() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/detail/cases.json");
    let cases: Vec<Case> = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
    assert!(cases.len() >= 40);
    for case in cases {
        let stage = Stage {
            width: case.image.width as u32,
            height: case.image.height as u32,
        };
        let input = build(&case.image);
        let result = run(
            &operation(case.parameters, stage, case.scale),
            &input,
            stage,
            Parallelism::Serial,
        );
        for (i, (mut actual, expected)) in result.into_iter().zip(case.expected).enumerate() {
            if case.coverage == 0.0 {
                actual = input[i];
            } else {
                for c in 0..3 {
                    actual[c] = (1.0 - case.coverage as f32) * input[i][c]
                        + case.coverage as f32 * actual[c];
                }
            }
            for c in 0..3 {
                assert!(
                    (f64::from(actual[c]) - expected[c]).abs() <= 1e-5 + 1e-5 * expected[c].abs(),
                    "{} pixel{i} channel{c}: {} != {}",
                    case.name,
                    actual[c],
                    expected[c]
                );
            }
        }
    }
}

#[test]
fn detail_serial_and_pool_and_region_are_exact() {
    let stage = Stage {
        width: 97,
        height: 61,
    };
    let p = [65.0, 3.0, 75.0, 40.0, 40.0, 50.0, 40.0, 50.0];
    let input = build(&Spec {
        width: 97,
        height: 61,
        kind: "noise".into(),
        seed: 4171,
    });
    for scale in [[1.0; 2], [0.51, 0.37]] {
        let op = operation(p, stage, scale);
        let serial = run(&op, &input, stage, Parallelism::Serial);
        let pool = run(&op, &input, stage, Parallelism::Pool);
        assert_eq!(serial, pool);
        let mut current = planar(&input);
        for unit in op.units() {
            let area = Region::whole(stage);
            let mut whole = vec![0.0; current.len()];
            let mut scratch = vec![0.0; (unit.scratch_bytes(stage) / 4) as usize];
            unit.apply(
                &Planes::new(stage, area, &current).unwrap(),
                &mut PlanesMut::new(stage, area, &mut whole).unwrap(),
                None,
                &mut scratch,
                Parallelism::Serial,
            )
            .unwrap();
            for out in [
                Region {
                    x0: 31,
                    y0: 19,
                    width: 29,
                    height: 21,
                },
                Region {
                    x0: 0,
                    y0: 0,
                    width: 17,
                    height: 13,
                },
                Region {
                    x0: 84,
                    y0: 48,
                    width: 13,
                    height: 13,
                },
            ] {
                let held = out.grown(unit.halo(stage), stage);
                let full = Planes::new(stage, area, &current).unwrap();
                let tile: Vec<[f32; 3]> = (held.y0..held.y1())
                    .flat_map(|y| (held.x0..held.x1()).map(move |x| full.pixel(x, y).unwrap()))
                    .collect();
                let tile = planar(&tile);
                let mut cut = vec![0.0; out.pixels() as usize * 3];
                unit.apply(
                    &Planes::new(stage, held, &tile).unwrap(),
                    &mut PlanesMut::new(stage, out, &mut cut).unwrap(),
                    None,
                    &mut scratch,
                    Parallelism::Serial,
                )
                .unwrap();
                let cut = pixels(&cut);
                let whole = Planes::new(stage, area, &whole).unwrap();
                for y in out.y0..out.y1() {
                    for x in out.x0..out.x1() {
                        assert_eq!(
                            cut[((y - out.y0) * out.width + x - out.x0) as usize],
                            whole.pixel(x, y).unwrap(),
                            "cut ({x},{y})"
                        );
                    }
                }
            }
            current = whole;
        }
    }
}

#[test]
fn detail_declared_halos_and_scratch_stay_within_caps() {
    let stage = Stage {
        width: 1000,
        height: 700,
    };
    for scale in [[1.0; 2], [0.999; 2], [0.5, 0.31], [0.125; 2]] {
        let op = operation(
            [150.0, 3.0, 100.0, 100.0, 100.0, 100.0, 100.0, 100.0],
            stage,
            scale,
        );
        assert!(op.summed_halo(stage) < 128);
        assert!(op.units()[0].halo(stage) <= filters::DENOISE_SAMPLED_HALO_MAX);
        assert!(op.units()[1].halo(stage) <= filters::SHARPEN_HALO_MAX);
        assert_eq!(op.units()[0].scratch_bytes(stage), 13 * 4 * 1000 * 700);
        assert_eq!(op.units()[1].scratch_bytes(stage), 6 * 4 * 1000 * 700);
    }
    assert_eq!(
        filters::scratch_bytes(
            Stage {
                width: u32::MAX,
                height: u32::MAX
            },
            13
        ),
        u64::MAX
    );
}

#[test]
fn detail_scratch_overflow_is_a_resource_limit() {
    let stage = Stage {
        width: u32::MAX,
        height: u32::MAX,
    };
    let op = operation(
        [50.0, 1.0, 25.0, 0.0, 40.0, 50.0, 40.0, 50.0],
        stage,
        [1.0; 2],
    );
    assert_eq!(op.units()[0].scratch_bytes(stage), u64::MAX);
    let error = SpatialPlan::new(&op, stage, Tiling::Fixed(u32::MAX)).unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::ResourceLimit);
    assert!(error.detail.contains("overflow"));
}

#[test]
fn detail_ancillary_fields_preserve_identity_without_units() {
    let stage = Stage {
        width: 17,
        height: 5,
    };
    let module = DetailModule::new();
    for payload in [
        json!({}),
        json!({"radius":2.5}),
        json!({"sharpen-detail":75,"luminance-detail":90,"colour-detail":5}),
    ] {
        let Processing::Spatial(op) = module
            .compile(DETAIL_EFFECT, 1, &payload, CompileStage::exact(stage))
            .unwrap()
        else {
            panic!("Detail is spatial")
        };
        assert_eq!(op.len(), 0);
    }
    let p = [40.0, 1.0, 25.0, 0.0, 40.0, 50.0, 40.0, 50.0];
    assert_eq!(operation(p, stage, [1.0; 2]).len(), 2);
    assert_eq!(operation(p, stage, [0.001; 2]).len(), 2);
}

#[test]
fn detail_constants_are_bit_identical_and_greys_stay_neutral() {
    let stage = Stage {
        width: 17,
        height: 9,
    };
    let p = [100.0, 3.0, 100.0, 0.0, 100.0, 50.0, 100.0, 50.0];
    for value in [-0.2, 0.0, 0.01, 0.18, 0.45, 1.4] {
        let input = vec![[value; 3]; 17 * 9];
        assert_eq!(
            run(
                &operation(p, stage, [1.0; 2]),
                &input,
                stage,
                Parallelism::Serial
            ),
            input
        );
    }
    let input = build(&Spec {
        width: 17,
        height: 9,
        kind: "grey-ramp".into(),
        seed: 61742,
    });
    for p in run(
        &operation(p, stage, [1.0; 2]),
        &input,
        stage,
        Parallelism::Serial,
    ) {
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
    }
}

#[test]
fn detail_multi_unit_tiles_match_whole_frame_bit_for_bit_and_reuse_scratch() {
    let stage = Stage {
        width: 97,
        height: 61,
    };
    let input = build(&Spec {
        width: 97,
        height: 61,
        kind: "noise".into(),
        seed: 66281,
    });
    for scale in [[1.0; 2], [0.51, 0.37]] {
        let op = operation(
            [65.0, 3.0, 75.0, 40.0, 40.0, 50.0, 40.0, 50.0],
            stage,
            scale,
        );
        let whole = run(&op, &input, stage, Parallelism::Serial);
        for side in [7, 16, 512] {
            assert_eq!(
                tiled(&op, &input, stage, side),
                whole,
                "side{side}, scale{scale:?}"
            );
        }
    }
}

#[test]
fn detail_has_no_global_estimate_or_full_frame_lab_allocation() {
    let stage = Stage {
        width: 6000,
        height: 4000,
    };
    let op = operation(
        [65.0, 3.0, 75.0, 40.0, 40.0, 50.0, 40.0, 50.0],
        stage,
        [1.0; 2],
    );
    assert!(op.units().iter().all(|u| u.estimate_key().is_none()));
    let plan = SpatialPlan::new(&op, stage, Tiling::Halo).unwrap();
    assert!(plan.working_set() < 32 * 1024 * 1024);
    let context = crate::RenderContext::new();
    assert!(context.spatial().concurrency(plan.working_set()) >= 1);
}

#[test]
fn detail_export_evaluates_exact_detail_once() {
    use crate::{Cancel, EditorService, Mutation};
    let path = luxforge_testbase::paths::temp_catalog("detail-exact-export");
    let mut service = EditorService::open(&path).unwrap();
    let asset = service
        .import(&luxforge_testbase::paths::jpeg())
        .unwrap()
        .asset
        .id;
    service
        .apply_action(
            &asset,
            Mutation {
                expected_revision: 0,
                request_id: "detail".into(),
                actor: "detail".into(),
            },
            "set-detail",
            json!({"sharpening":50,"luminance":40,"colour":40}),
        )
        .unwrap();
    let plan = service.export_plan(&asset, None).unwrap();
    let context = plan.evaluation.context();
    let (before_compiles, before_frames) = (context.compiles(), context.spatial_frames());
    let frame = plan
        .evaluation
        .exact(&Cancel::never())
        .unwrap()
        .frame(plan.identity.snapshot_id.clone())
        .unwrap();
    assert_eq!(
        context.compiles(),
        before_compiles,
        "export borrows its one compilation"
    );
    assert_eq!(
        context.spatial_frames(),
        before_frames + 1,
        "one Detail operation frame, with denoise then sharpen"
    );
    let expected = service
        .render_entry(&asset, &plan.identity.entry_id)
        .unwrap();
    assert_eq!(
        frame.rgba, expected.rgba,
        "exact buffer before JPEG encoding"
    );
    drop((plan, service));
    fs::remove_file(path).unwrap();
}

#[test]
fn detail_cancelled_render_publishes_nothing_and_releases_scratch() {
    let path = luxforge_testbase::paths::jpeg();
    let source = crate::open_source(&path).unwrap();
    let context = crate::RenderContext::new();
    let registry = crate::ModuleRegistry::builtin();
    let recipe = crate::Recipe {
        layers: vec![crate::Layer::new(
            DETAIL_EFFECT,
            json!({"sharpening":50,"luminance":40,"colour":40}),
        )],
        ..crate::Recipe::default()
    };
    let cancel = crate::Cancel::new();
    cancel.cancel();
    let error = crate::render(
        &registry,
        &source,
        &recipe,
        crate::RenderOptions::exact(&cancel),
        &context,
    )
    .unwrap()
    .frame(crate::SnapshotId::new())
    .unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Cancelled);
    assert_eq!(context.spatial().in_use(), 0);
    assert_eq!(context.scratch().in_use(), 0);
    let frame = crate::render(
        &registry,
        &source,
        &recipe,
        crate::RenderOptions::default(),
        &context,
    )
    .unwrap()
    .frame(crate::SnapshotId::new())
    .unwrap();
    assert_eq!(frame.width, source.width);
    assert_eq!(context.spatial().in_use(), 0);
}

#[test]
fn detail_cancellation_inside_the_first_tile_releases_scratch() {
    let source = crate::render::tests::gradient(64, 48);
    let context = crate::RenderContext::new();
    let registry = crate::ModuleRegistry::builtin();
    let recipe = crate::Recipe {
        layers: vec![crate::Layer::new(
            DETAIL_EFFECT,
            json!({"sharpening":50,"luminance":40,"colour":40}),
        )],
        ..crate::Recipe::default()
    };
    let cancel = crate::Cancel::new();
    let evaluation = crate::render(
        &registry,
        &source,
        &recipe,
        crate::RenderOptions::exact(&cancel),
        &context,
    )
    .unwrap();
    filters::cancel_after_levels(Some(1));
    let error = evaluation.frame(crate::SnapshotId::new()).unwrap_err();
    assert_eq!(
        filters::finished_levels(),
        1,
        "stops after the first wavelet level"
    );
    filters::cancel_after_levels(None);
    assert_eq!(error.kind, crate::ErrorKind::Cancelled);
    assert_eq!(context.spatial().in_use(), 0);
    assert_eq!(context.scratch().in_use(), 0);
    // Reusing the same budgets after the refusal proves neither the tile nor its scratch stays
    // reserved, and a new evaluation has no partially published output to reuse.
    let frame = crate::render(
        &registry,
        &source,
        &recipe,
        crate::RenderOptions::default(),
        &context,
    )
    .unwrap()
    .frame(crate::SnapshotId::new())
    .unwrap();
    assert_eq!((frame.width, frame.height), (64, 48));
    assert_eq!(context.spatial().in_use(), 0);
}

#[test]
fn detail_cancellation_stops_row_chunks() {
    let stage = Stage {
        width: 32,
        height: 24,
    };
    let geometry = filters::Geometry {
        stage,
        held: Region::whole(stage),
    };
    let mut plane = vec![0.0; stage.width as usize * stage.height as usize];
    let cancel = crate::Cancel::new();
    let written = std::sync::atomic::AtomicUsize::new(0);
    let error = filters::rows(
        &mut plane,
        geometry,
        Region::whole(stage),
        Parallelism::Serial,
        &cancel,
        |_, row| {
            row.fill(1.0);
            written.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            cancel.cancel();
        },
    )
    .unwrap_err();
    assert_eq!(error.kind, crate::ErrorKind::Cancelled);
    assert_eq!(written.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(plane[..32].iter().all(|v| *v == 1.0));
    assert!(plane[32..].iter().all(|v| *v == 0.0));
}
