//! Exact geometry and the resample: composed orientation and crops against stepwise and
//! independent references, read rectangles, and samples equal to the rendered bytes.

use super::byte::{band, source_pixel};
use super::testing::{render, sample};
use super::tests::*;
use super::*;
use crate::{
    AssetId, EFFECT_FORMAT, ErrorKind, Layer, LayerId, Orientation, Recipe, Snapshot, SnapshotId,
    SourceImage, Transform,
    modules::{BoxRect, CropPayload, CropStage, Region, Resample, Stage},
};
use serde_json::json;
use std::sync::Arc;

/// The one read-rectangle rule holds every tap a resample's output window reads, in a frame at
/// the stage's origin or a window of it, at any angle and scale, including taps clamped to an
/// edge the output maps beyond; it never leaves the input and answers nothing it cannot bound.
#[test]
fn a_resample_reads_every_tap_of_its_window_inside_one_rectangle() {
    let stage = Stage {
        width: 97,
        height: 61,
    };
    let (output_width, output_height) = (40, 30);
    let windows = [
        Region {
            x0: 0,
            y0: 0,
            width: output_width,
            height: output_height,
        },
        Region {
            x0: 3,
            y0: 5,
            width: 7,
            height: 1,
        },
        Region {
            x0: output_width - 1,
            y0: output_height - 1,
            width: 1,
            height: 1,
        },
        Region {
            x0: 10,
            y0: 0,
            width: 30,
            height: 17,
        },
    ];
    for (angle, scale) in [
        (0.0, 1.0),
        (7.0, 1.0),
        (-30.0, 0.8),
        (45.0, 2.5),
        (90.0, 1.0),
        (13.0, 0.37),
        (200.0, 4.0),
    ] {
        let (sin, cos): (f64, f64) = f64::to_radians(angle).sin_cos();
        // Output centre (20, 15) onto input centre (48.5, 30.5), rotated and scaled.
        let resample = Resample {
            inverse: [
                scale * cos,
                -scale * sin,
                48.5 - scale * (cos * 20.0 - sin * 15.0),
                scale * sin,
                scale * cos,
                30.5 - scale * (sin * 20.0 + cos * 15.0),
            ],
            output_width,
            output_height,
        };
        for origin in [(0, 0), (5, 3), (40, 20)] {
            let frame = Stage {
                width: stage.width - origin.0,
                height: stage.height - origin.1,
            };
            for window in windows {
                let read = resample
                    .reads(origin, window, frame)
                    .expect("a finite mapping is bounded");
                assert!(
                    !read.is_empty() && read.x1() <= frame.width && read.y1() <= frame.height,
                    "{angle}° ×{scale} {origin:?} {window:?}: {read:?} leaves the frame"
                );
                for y in window.y0..window.y1() {
                    for x in window.x0..window.x1() {
                        let (u, v) = resample.input_from(origin, x, y);
                        for (tap_x, tap_y) in Taps::new(u, v, frame.width, frame.height).corners {
                            assert!(
                                read.contains(tap_x, tap_y),
                                "{angle}° ×{scale} {origin:?} {window:?}: tap \
                                 ({tap_x}, {tap_y}) of ({x}, {y}) is outside {read:?}"
                            );
                        }
                    }
                }
            }
        }
    }
    let unbounded = Resample {
        inverse: [f64::NAN, 0.0, 0.0, 0.0, 1.0, 0.0],
        output_width,
        output_height,
    };
    assert_eq!(unbounded.reads((0, 0), windows[0], stage), None);
    let empty = Region {
        width: 0,
        ..windows[0]
    };
    let identity = Resample {
        inverse: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        output_width,
        output_height,
    };
    assert_eq!(identity.reads((0, 0), empty, stage), None);
}

/// The colour pass before a crop covers only the rows the crop reads, so the rendered frame
/// must still agree with the point sampler at every output pixel, and the band must be a
/// strict subset of the stage for a crop that discards rows.
#[test]
fn colour_before_a_crop_is_applied_only_where_the_crop_reads_and_stays_exact() {
    let registry = registry();
    let source = source(240, 320);
    let stage = CropStage {
        width: 320,
        height: 240,
        angle: 7.0,
    };
    let (box_width, box_height) = stage.bounding_box();
    // A wide, short crop near the centre, so whole rows above and below it are never read.
    let rect = BoxRect {
        x: box_width * 0.1,
        y: box_height * 0.35,
        width: box_width * 0.8,
        height: box_height * 0.3,
    };
    let layers = vec![
        turn(Transform::RotateRight),
        Layer {
            id: LayerId::new(),
            effect_id: crate::BASIC_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({"exposure": 0.7, "contrast": 30.0, "vibrance": 40.0}),
            mask: None,
            artifacts: Vec::new(),
        },
        Layer::crop(rect.normalized(&stage)),
    ];
    let recipe = Recipe {
        format: crate::RECIPE_FORMAT,
        layers,
        masks: Vec::new(),
        ..Recipe::default()
    };
    let compiled = registry
        .compile(source.width, source.height, &recipe)
        .unwrap();
    assert!(
        compiled.segments[1]
            .entry
            .as_ref()
            .is_some_and(Entry::blends),
        "a rotated crop resamples"
    );
    let band = band(&compiled, 0);
    assert!(
        band.start > 0 && band.end < compiled.segments[0].height as usize,
        "the band {band:?} should exclude rows of the {} high stage",
        compiled.segments[0].height
    );
    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
    for y in 0..raster.height {
        for x in 0..raster.width {
            let sampled = sample(&registry, &source, &recipe, x, y).unwrap();
            assert_eq!(
                raster.pixel(x, y),
                sampled.rgba,
                "pixel ({x}, {y}) differs from the sampler"
            );
        }
    }
}

fn red(raster: &Raster) -> Vec<u8> {
    raster.rgba.chunks_exact(4).map(|p| p[0]).collect()
}

fn offset_layer(x: i64, y: i64, width: u32, height: u32) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: TEST_OFFSET_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({"x": x, "y": y, "width": width, "height": height}),
        mask: None,
        artifacts: Vec::new(),
    }
}

#[test]
#[ignore = "measurement, run explicitly in release"]
fn measure_resample_on_photo_sized_frames() {
    let registry = geometry_registry();
    for (width, height) in [(6000_u32, 4000_u32), (9504, 6336)] {
        let source = gradient(width, height);
        for (label, layers) in [
            ("exact rotate", vec![turn(Transform::RotateRight)]),
            (
                "crop 0 deg",
                vec![crop_layer(fitted_crop(
                    width,
                    height,
                    0.0,
                    [0.1, 0.1, 0.8, 0.8],
                ))],
            ),
            (
                "crop 10 deg",
                vec![crop_layer(fitted_crop(
                    width,
                    height,
                    10.0,
                    [0.1, 0.1, 0.8, 0.8],
                ))],
            ),
            (
                "crop 10 deg then rotate",
                vec![
                    crop_layer(fitted_crop(width, height, 10.0, [0.1, 0.1, 0.8, 0.8])),
                    turn(Transform::RotateRight),
                ],
            ),
        ] {
            let recipe = Recipe {
                format: crate::RECIPE_FORMAT,
                layers,
                masks: Vec::new(),
                ..Recipe::default()
            };
            let mut best = f64::INFINITY;
            let mut raster = None;
            for _ in 0..5 {
                let start = std::time::Instant::now();
                raster = Some(render(&registry, &source, SnapshotId::new(), &recipe).unwrap());
                best = best.min(start.elapsed().as_secs_f64() * 1000.0);
            }
            let raster = raster.unwrap();
            println!(
                "{width}x{height} {label}: {best:.1} ms -> {}x{}",
                raster.width, raster.height
            );
        }
    }
}

#[test]
fn rotated_crops_match_an_independent_reference_sampler() {
    let registry = geometry_registry();
    // Every case runs serially and forced onto the parallel row path; every pixel of every case
    // is compared against the reference.
    for (pooled, (width, height, angle, rect)) in [false, true].into_iter().flat_map(|pooled| {
        [
            (64_u32, 48_u32, 7.5_f64, [0.12, 0.1, 0.7, 0.75]),
            (64, 48, -30.0, [0.25, 0.2, 0.5, 0.55]),
            (64, 48, 45.0, [0.3, 0.3, 0.4, 0.4]),
            (150, 110, 10.0, [0.1, 0.1, 0.8, 0.8]),
        ]
        .map(|case| (pooled, case))
    }) {
        parallel::force(Some(pooled));
        let source = gradient(width, height);
        let crop = fitted_crop(width, height, angle, rect);
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![crop_layer(crop)],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        let reference = CropReference::new(&source, crop);
        let case = format!("{width}x{height} at {angle}, pooled {pooled}");
        assert_eq!(
            (raster.width, raster.height),
            (reference.width, reference.height),
            "{case}: output dimensions"
        );
        assert!(raster.width > 1 && raster.height > 1, "{case}");
        let mut worst = 0_i32;
        for j in 0..raster.height {
            for i in 0..raster.width {
                let expected = reference.pixel(&source, i, j);
                let actual = raster.pixel(i, j).expect("inside the output stage");
                for channel in 0..4 {
                    let difference = i32::from(actual[channel]) - i32::from(expected[channel]);
                    worst = worst.max(difference.abs());
                    assert!(
                        difference.abs() <= 1,
                        "{case}: pixel ({i}, {j}) channel {channel}: {actual:?} against {expected:?}"
                    );
                }
            }
        }
        println!("{case}: worst channel difference against the f64 reference is {worst}");
    }
    parallel::force(None);
}

#[test]
fn angle_zero_crops_are_exact_copies_composed_into_one_pass() {
    let registry = geometry_registry();
    let source = source(7, 5);
    let crops = [
        CropPayload::NEUTRAL,
        CropPayload {
            angle: 0.0,
            x: 2.0 / 7.0,
            y: 1.0 / 5.0,
            width: 4.0 / 7.0,
            height: 3.0 / 5.0,
        },
        CropPayload {
            angle: 0.0,
            x: 0.0,
            y: 4.0 / 5.0,
            width: 1.0,
            height: 1.0 / 5.0,
        },
    ];
    for crop in crops {
        for stack in [
            vec![],
            vec![Transform::RotateRight],
            vec![Transform::MirrorHorizontal],
        ] {
            for after in [
                vec![],
                vec![Transform::RotateLeft],
                vec![Transform::FlipVertical],
            ] {
                let mut layers: Vec<Layer> = stack.iter().copied().map(turn).collect();
                layers.push(crop_layer(crop));
                layers.extend(after.iter().copied().map(turn));
                let recipe = Recipe {
                    format: crate::RECIPE_FORMAT,
                    layers: layers.clone(),
                    masks: Vec::new(),
                    ..Recipe::default()
                };
                let compiled = registry
                    .compile(source.width, source.height, &recipe)
                    .unwrap();
                assert_eq!(
                    compiled.segments.len(),
                    1,
                    "an exact crop never starts a new rasterizing pass"
                );
                let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
                let expected = reference(&source, &layers);
                assert_eq!(
                    (raster.width, raster.height),
                    (expected.0, expected.1),
                    "{layers:?}"
                );
                assert_eq!(
                    raster.rgba.as_slice(),
                    expected.2,
                    "{crop:?} {stack:?} {after:?}"
                );
            }
        }
    }
    // A neutral crop of the whole stage still shares the source allocation.
    let shared = render(
        &registry,
        &source,
        SnapshotId::new(),
        &Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![crop_layer(CropPayload::NEUTRAL)],
            masks: Vec::new(),
            ..Recipe::default()
        },
    )
    .unwrap();
    assert!(Arc::ptr_eq(&shared.rgba, &source.rgba));
}

#[test]
fn a_translation_with_a_smaller_output_composes_and_is_bounds_checked() {
    let registry = geometry_registry();
    let source = source(7, 5);
    // Two translations and a quarter turn compose into one mapping over the source.
    let layers = vec![
        offset_layer(1, 1, 5, 4),
        turn(Transform::RotateRight),
        offset_layer(1, 2, 2, 3),
    ];
    let recipe = Recipe {
        format: crate::RECIPE_FORMAT,
        layers: layers.clone(),
        masks: Vec::new(),
        ..Recipe::default()
    };
    let compiled = registry
        .compile(source.width, source.height, &recipe)
        .unwrap();
    assert_eq!(compiled.segments.len(), 1);
    let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
    assert_eq!((raster.width, raster.height), (2, 3));
    // The same stack applied one step at a time, through the composed mapping's own definition.
    let mut expected = Vec::new();
    for y in 0..raster.height {
        for x in 0..raster.width {
            let (turned_x, turned_y) = (x + 1, y + 2);
            let (offset_x, offset_y) = (turned_y, 4 - 1 - turned_x);
            expected.extend(source_pixel(&source, offset_x + 1, offset_y + 1));
        }
    }
    assert_eq!(raster.rgba.as_slice(), expected);
    for (x, y) in [(0, 0), (raster.width - 1, raster.height - 1)] {
        assert_eq!(
            sample(&registry, &source, &recipe, x, y).unwrap().rgba,
            raster.pixel(x, y)
        );
    }
    // A mapping that would read outside its input frame is refused, not rasterized.
    for layers in [
        vec![offset_layer(3, 0, 5, 5)],
        vec![offset_layer(-1, 0, 4, 4)],
        vec![offset_layer(0, 0, 8, 5)],
        vec![offset_layer(1, 1, 5, 4), offset_layer(1, 0, 5, 4)],
    ] {
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: layers.clone(),
            masks: Vec::new(),
            ..Recipe::default()
        };
        let error = render(&registry, &source, SnapshotId::new(), &recipe)
            .expect_err(&format!("{layers:?} reads outside the stage"));
        assert_eq!(error.kind, ErrorKind::Validation);
        assert!(error.detail.contains("reads outside"), "{error}");
        assert_eq!(
            sample(&registry, &source, &recipe, 0, 0).unwrap_err().kind,
            ErrorKind::Validation
        );
    }
}

#[test]
fn a_point_replacement_cropped_away_simply_disappears() {
    let registry = geometry_registry();
    let source = source(8, 6);
    let inside = Layer::pixel(4, 3, [250, 1, 2]);
    let outside = Layer::pixel(0, 0, [3, 251, 4]);
    for crop in [
        crop_layer(CropPayload {
            angle: 0.0,
            x: 0.25,
            y: 1.0 / 3.0,
            width: 0.5,
            height: 0.5,
        }),
        crop_layer(fitted_crop(8, 6, 15.0, [0.25, 0.25, 0.5, 0.5])),
    ] {
        let with_outside = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![inside.clone(), outside.clone(), crop.clone()],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let without = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: vec![inside.clone(), crop.clone()],
            masks: Vec::new(),
            ..Recipe::default()
        };
        let rendered = render(&registry, &source, SnapshotId::new(), &with_outside).unwrap();
        let expected = render(&registry, &source, SnapshotId::new(), &without).unwrap();
        assert_eq!(rendered.rgba, expected.rgba, "{crop:?}");
        for y in 0..rendered.height {
            for x in 0..rendered.width {
                assert_eq!(
                    sample(&registry, &source, &with_outside, x, y)
                        .unwrap()
                        .rgba,
                    rendered.pixel(x, y),
                    "({x}, {y})"
                );
            }
        }
    }
}

#[test]
fn the_frame_limit_applies_to_a_resample_and_point_queries_still_answer() {
    let registry = geometry_registry();
    let source = source(3, 2);
    let recipe = Recipe {
        format: crate::RECIPE_FORMAT,
        layers: vec![scale_layer(10_000.0)],
        masks: Vec::new(),
        ..Recipe::default()
    };
    let error = render(&registry, &source, SnapshotId::new(), &recipe)
        .expect_err("30000x20000 is over the frame limit");
    assert_eq!(error.kind, ErrorKind::ResourceLimit);
    assert!(error.detail.contains("512 MiB"), "{error}");
    // The same stack answers a point query, because no frame is allocated for one pixel.
    let sampled = sample(&registry, &source, &recipe, 15_000, 10_000).unwrap();
    assert_eq!((sampled.width, sampled.height), (30_000, 20_000));
    assert!(sampled.rgba.is_some());
    // A resample must declare a stage the host can address at all.
    let empty = Recipe {
        format: crate::RECIPE_FORMAT,
        layers: vec![scale_layer(0.0)],
        masks: Vec::new(),
        ..Recipe::default()
    };
    let error = sample(&registry, &source, &empty, 0, 0).unwrap_err();
    assert_eq!(error.kind, ErrorKind::Validation);
}

#[test]
fn pixel_layers_are_ordered_exact_and_source_is_immutable() {
    let source = source(3, 2);
    let original = source.clone();
    let a = Layer::pixel(1, 0, [200, 201, 202]);
    let first = rendered(&source, vec![a.clone()]);
    let second = rendered(&source, vec![a, Layer::pixel(1, 0, [9, 8, 7])]);
    assert_eq!(first.pixel(1, 0), Some([200, 201, 202, 255]));
    assert_eq!(second.pixel(1, 0), Some([9, 8, 7, 255]));
    assert_eq!(first.pixel(0, 0), Some([0, 20, 40, 255]));
    assert_eq!(source, original);
    assert!(rendered(&source, vec![]).pixel(1, 0) != second.pixel(1, 0));
}

#[test]
fn exact_transform_coordinate_tables_for_asymmetric_input() {
    let source = source(3, 2);
    assert_eq!(
        red(&rendered(&source, vec![turn(Transform::RotateRight)])),
        vec![3, 0, 4, 1, 5, 2]
    );
    assert_eq!(
        red(&rendered(&source, vec![turn(Transform::RotateLeft)])),
        vec![2, 5, 1, 4, 0, 3]
    );
    assert_eq!(
        red(&rendered(&source, vec![turn(Transform::MirrorHorizontal)])),
        vec![2, 1, 0, 5, 4, 3]
    );
    assert_eq!(
        red(&rendered(&source, vec![turn(Transform::FlipVertical)])),
        vec![3, 4, 5, 0, 1, 2]
    );
}

#[test]
fn transform_identities_and_operation_order_hold() {
    let source = source(5, 3);
    for (transform, count) in [
        (Transform::RotateRight, 4),
        (Transform::RotateLeft, 4),
        (Transform::MirrorHorizontal, 2),
        (Transform::FlipVertical, 2),
    ] {
        let layers = (0..count).map(|_| turn(transform)).collect();
        assert_eq!(rendered(&source, layers).rgba, source.rgba);
        // The same actions composed into one layer reach the neutral orientation, whose
        // identity mapping shares the source buffer instead of copying it.
        let mut composed = Orientation::NEUTRAL;
        for _ in 0..count {
            composed = composed.then(transform);
        }
        assert_eq!(composed, Orientation::NEUTRAL, "{transform:?}");
        let collapsed = rendered(&source, vec![Layer::orientation(composed)]);
        assert_eq!(collapsed.rgba, source.rgba);
        assert!(Arc::ptr_eq(&collapsed.rgba, &source.rgba));
    }
    let before = rendered(
        &source,
        vec![
            Layer::pixel(0, 0, [250, 0, 0]),
            turn(Transform::RotateRight),
        ],
    );
    assert_eq!(before.pixel(2, 0), Some([250, 0, 0, 255]));
    let after = rendered(
        &source,
        vec![
            turn(Transform::RotateRight),
            Layer::pixel(0, 0, [250, 0, 0]),
        ],
    );
    assert_eq!(after.pixel(0, 0), Some([250, 0, 0, 255]));
    assert_ne!(before.rgba, after.rgba);
}

#[test]
fn compiled_recipes_match_stepwise_evaluation_for_interleaved_operations() {
    let source = source(5, 3);
    let transforms = [
        Transform::RotateLeft,
        Transform::RotateRight,
        Transform::MirrorHorizontal,
        Transform::FlipVertical,
    ];
    for first in transforms {
        for second in transforms {
            for third in transforms {
                let layers = vec![
                    Layer::pixel(1, 1, [201, 1, 2]),
                    turn(first),
                    Layer::pixel(0, 0, [3, 202, 4]),
                    turn(second),
                    Layer::pixel(1, 1, [5, 6, 203]),
                    turn(third),
                    Layer::pixel(0, 0, [204, 8, 9]),
                ];
                let expected = reference(&source, &layers);
                let actual = rendered(&source, layers);
                assert_eq!((actual.width, actual.height), (expected.0, expected.1));
                assert_eq!(actual.rgba.as_slice(), expected.2);
            }
        }
    }
}

/// One orientation layer holding the composed state renders exactly what the same actions
/// render as separate single-action layers, and both match the stepwise reference. Every one
/// of the eight orientations against every action, and every sequence of three actions, on a
/// non-square source so a wrongly composed quarter turn changes the dimensions.
#[test]
fn a_composed_orientation_renders_what_its_separate_action_layers_render() {
    let source = source(5, 3);
    let transforms = [
        Transform::RotateLeft,
        Transform::RotateRight,
        Transform::MirrorHorizontal,
        Transform::FlipVertical,
    ];
    let orientations = [false, true]
        .into_iter()
        .flat_map(|mirror| (0..4).map(move |turns| Orientation { mirror, turns }));
    for state in orientations {
        for transform in transforms {
            let separate = vec![Layer::orientation(state), turn(transform)];
            let expected = reference(&source, &separate);
            let collapsed = rendered(&source, vec![Layer::orientation(state.then(transform))]);
            let stepwise = rendered(&source, separate);
            for raster in [&collapsed, &stepwise] {
                assert_eq!(
                    (raster.width, raster.height),
                    (expected.0, expected.1),
                    "{state:?} then {transform:?}"
                );
                assert_eq!(
                    raster.rgba.as_slice(),
                    expected.2,
                    "{state:?} then {transform:?}"
                );
            }
        }
    }
    for first in transforms {
        for second in transforms {
            for third in transforms {
                let separate = vec![turn(first), turn(second), turn(third)];
                let composed = Orientation::of(first).then(second).then(third);
                let expected = reference(&source, &separate);
                let collapsed = rendered(&source, vec![Layer::orientation(composed)]);
                assert_eq!(
                    (collapsed.width, collapsed.height),
                    (expected.0, expected.1),
                    "{first:?} {second:?} {third:?}"
                );
                assert_eq!(
                    collapsed.rgba.as_slice(),
                    expected.2,
                    "{first:?} {second:?} {third:?}"
                );
                assert_eq!(rendered(&source, separate).rgba, collapsed.rgba);
            }
        }
    }
}

#[test]
fn invalid_coordinates_and_buffers_fail_without_panicking() {
    let registry = registry();
    let source = source(3, 2);
    let snapshot = Snapshot::original(AssetId::new());
    let outside = Recipe {
        format: crate::RECIPE_FORMAT,
        layers: vec![Layer::pixel(3, 0, [0, 0, 0])],
        masks: Vec::new(),
        ..Recipe::default()
    };
    assert!(render(&registry, &source, snapshot.id.clone(), &outside).is_err());
    assert!(
        sample(&registry, &source, &outside, 0, 0).is_err(),
        "a sample of the same stack fails too"
    );
    let malformed = SourceImage {
        rgba: vec![0].into(),
        ..source
    };
    assert!(render(&registry, &malformed, snapshot.id, &Recipe::default()).is_err());
}
