//! Locating an output pixel in the content stage, and the stage transform in both directions.

use super::byte::source_pixel;
use super::testing::{extents, render};
use super::tests::*;
use super::*;
use crate::{
    EFFECT_FORMAT, Error, ErrorKind, Layer, LayerId, ORIENTATION_EFFECT, Orientation, PIXEL_EFFECT,
    Recipe, SnapshotId, Transform,
    modules::{CropPayload, ModuleRegistry},
};
use serde_json::json;

/// [`locate`] through a fresh compilation of `recipe` against a `width` × `height` stage.
fn locate_recipe(
    registry: &ModuleRegistry,
    width: u32,
    height: u32,
    recipe: &Recipe,
    x: u32,
    y: u32,
) -> Result<ContentPoint, Error> {
    locate(
        &registry.compile(width, height, recipe)?,
        width,
        height,
        x,
        y,
    )
}

/// Where one pixel of a stage lands after the exact layers that follow it, evaluated one layer
/// at a time and independently of the renderer: the direction `locate` walks
/// backwards. `None` when a crop discards it. Pixel layers move nothing, so they are skipped.
fn forward(width: u32, height: u32, layers: &[Layer], x: u32, y: u32) -> Option<(u32, u32)> {
    let (mut width, mut height, mut x, mut y) = (width, height, x, y);
    for layer in layers {
        match layer.effect_id.as_str() {
            PIXEL_EFFECT => {}
            ORIENTATION_EFFECT => {
                for transform in steps(&layer.payload) {
                    (x, y) = match transform {
                        Transform::RotateRight => (height - 1 - y, x),
                        Transform::RotateLeft => (y, width - 1 - x),
                        Transform::MirrorHorizontal => (width - 1 - x, y),
                        Transform::FlipVertical => (x, height - 1 - y),
                    };
                    (width, height) = match transform {
                        Transform::RotateLeft | Transform::RotateRight => (height, width),
                        Transform::MirrorHorizontal | Transform::FlipVertical => (width, height),
                    };
                }
            }
            TEST_CROP_EFFECT => {
                let crop: CropPayload = serde_json::from_value(layer.payload.clone()).unwrap();
                assert_eq!(crop.angle, 0.0, "the stepwise reference never straightens");
                let (origin_x, origin_y) = (
                    (crop.x * f64::from(width)).round() as u32,
                    (crop.y * f64::from(height)).round() as u32,
                );
                width = (crop.width * f64::from(width)).round().max(1.0) as u32;
                height = (crop.height * f64::from(height)).round().max(1.0) as u32;
                if x < origin_x || y < origin_y || x >= origin_x + width || y >= origin_y + height {
                    return None;
                }
                (x, y) = (x - origin_x, y - origin_y);
            }
            other => panic!("unexpected test effect {other}"),
        }
    }
    Some((x, y))
}

/// The pixel index a continuous index-space position is nearest to, clamped to the frame.
fn nearest(position: f64, limit: u32) -> u32 {
    position.round().clamp(0.0, f64::from(limit - 1)) as u32
}

fn degenerate_layer() -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: TEST_DEGENERATE_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({}),
        mask: None,
        artifacts: Vec::new(),
    }
}

#[test]
fn locating_maps_exact_geometry_back_to_the_content_pixel() {
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
        for before in [
            vec![],
            vec![Transform::RotateRight],
            vec![Transform::MirrorHorizontal],
            vec![Transform::RotateLeft, Transform::FlipVertical],
        ] {
            for after in [
                vec![],
                vec![Transform::RotateLeft],
                vec![Transform::FlipVertical],
                vec![Transform::MirrorHorizontal, Transform::RotateRight],
            ] {
                let mut layers: Vec<Layer> = before.iter().copied().map(turn).collect();
                layers.push(crop_layer(crop));
                layers.extend(after.iter().copied().map(turn));
                let recipe = Recipe {
                    format: crate::RECIPE_FORMAT,
                    layers: layers.clone(),
                    masks: Vec::new(),
                    ..Recipe::default()
                };
                let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
                let locate =
                    |x, y| locate_recipe(&registry, source.width, source.height, &recipe, x, y);
                // Every output pixel names a content pixel that the stepwise forward map puts
                // back where it was found, and the rendered bytes are that content pixel's.
                for y in 0..raster.height {
                    for x in 0..raster.width {
                        let located = locate(x, y).unwrap();
                        let content = (located.content_x, located.content_y);
                        assert_eq!(
                            (located.width, located.height),
                            (source.width, source.height),
                            "{layers:?}"
                        );
                        assert_eq!(
                            forward(source.width, source.height, &layers, content.0, content.1),
                            Some((x, y)),
                            "({x}, {y}) of {layers:?}"
                        );
                        assert_eq!(
                            raster.pixel(x, y),
                            Some(source_pixel(&source, content.0, content.1)),
                            "({x}, {y}) of {layers:?}"
                        );
                    }
                }
                // And every content pixel the stack keeps is located from where it lands.
                for y in 0..source.height {
                    for x in 0..source.width {
                        let Some((out_x, out_y)) =
                            forward(source.width, source.height, &layers, x, y)
                        else {
                            continue;
                        };
                        let located = locate(out_x, out_y).unwrap();
                        assert_eq!(
                            (located.content_x, located.content_y),
                            (x, y),
                            "({x}, {y}) of {layers:?}"
                        );
                    }
                }
                // A point outside the output stage is refused, not clamped.
                for (x, y) in [(raster.width, 0), (0, raster.height)] {
                    let error =
                        locate(x, y).expect_err(&format!("({x}, {y}) is outside {layers:?}"));
                    assert_eq!(error.kind, ErrorKind::Validation);
                    assert!(
                        error
                            .detail
                            .contains(&format!("{}x{}", raster.width, raster.height)),
                        "{error}"
                    );
                }
            }
        }
    }
}

#[test]
fn a_rotated_crop_locates_the_nearest_pixel_its_sampler_read() {
    let registry = geometry_registry();
    for (width, height, angle, rect, after) in [
        (40_u32, 24_u32, 12.0_f64, [0.2, 0.15, 0.6, 0.65], vec![]),
        (
            28,
            36,
            -30.0,
            [0.25, 0.2, 0.5, 0.55],
            vec![Transform::RotateRight],
        ),
        (
            32,
            24,
            7.5,
            [0.3, 0.25, 0.45, 0.5],
            vec![Transform::MirrorHorizontal, Transform::FlipVertical],
        ),
    ] {
        let source = gradient(width, height);
        let crop = fitted_crop(width, height, angle, rect);
        let tail: Vec<Layer> = after.iter().copied().map(turn).collect();
        // A pixel layer before the crop moves nothing; the walk back is geometry only.
        let mut layers = vec![Layer::pixel(2, 3, [250, 1, 2]), crop_layer(crop)];
        layers.extend(tail.iter().cloned());
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers: layers.clone(),
            masks: Vec::new(),
            ..Recipe::default()
        };
        let raster = render(&registry, &source, SnapshotId::new(), &recipe).unwrap();
        let reference = CropReference::new(&source, crop);
        let case = format!("{width}x{height} at {angle}");
        assert_eq!(
            u64::from(raster.width) * u64::from(raster.height),
            u64::from(reference.width) * u64::from(reference.height),
            "{case}: output pixel count"
        );
        for j in 0..reference.height {
            for i in 0..reference.width {
                let (x, y) = forward(reference.width, reference.height, &tail, i, j)
                    .expect("exact transforms keep every pixel");
                let located =
                    locate_recipe(&registry, source.width, source.height, &recipe, x, y).unwrap();
                let (u, v) = reference.position(&source, i, j);
                assert_eq!(
                    (located.content_x, located.content_y),
                    (nearest(u, width), nearest(v, height)),
                    "{case}: ({i}, {j}) of the crop's output"
                );
                assert_eq!((located.width, located.height), (width, height), "{case}");
            }
        }
    }
}

/// The 64-bit LCG the randomized geometry points draw from. Fixed seeds, so a disagreement is
/// the same disagreement on every host and run.
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

/// The stage a tail's transforms hand the crop after them.
fn turned_stage(width: u32, height: u32, transforms: &[Transform]) -> (u32, u32) {
    let (mut width, mut height) = (width, height);
    for transform in transforms {
        if matches!(transform, Transform::RotateLeft | Transform::RotateRight) {
            (width, height) = (height, width);
        }
    }
    (width, height)
}

/// Every geometry tail the delivered modules produce: nothing, each of the eight orientations, a
/// crop at zero degrees, a straightened crop, and crops with transforms on both sides. A pixel,
/// a colour and a spatial layer ride along in one case each, because they move no coordinate and
/// the composition must ignore them while still walking the segments they open.
///
/// The flag is whether the tail resamples, which is the only thing that costs the mapping its
/// exactness: a straightened crop, and nothing else.
fn geometry_tails(width: u32, height: u32) -> Vec<(String, Vec<Layer>, bool)> {
    let mut tails = vec![("no transform".to_owned(), Vec::new(), false)];
    for turns in 0..4u8 {
        for mirror in [false, true] {
            tails.push((
                format!("orientation mirror={mirror} turns={turns}"),
                vec![Layer::orientation(Orientation { mirror, turns })],
                false,
            ));
        }
    }
    let crops: [(f64, [f64; 4]); 3] = [
        (0.0, [0.15, 0.2, 0.6, 0.55]),
        (11.0, [0.2, 0.15, 0.6, 0.65]),
        (-37.5, [0.25, 0.2, 0.5, 0.5]),
    ];
    let arrangements: [(&[Transform], &[Transform]); 4] = [
        (&[], &[]),
        (&[Transform::RotateRight], &[]),
        (&[], &[Transform::MirrorHorizontal, Transform::RotateLeft]),
        (
            &[Transform::MirrorHorizontal, Transform::RotateLeft],
            &[Transform::FlipVertical],
        ),
    ];
    for (angle, rect) in crops {
        for (before, after) in arrangements {
            let (crop_width, crop_height) = turned_stage(width, height, before);
            let mut layers: Vec<Layer> = before.iter().copied().map(turn).collect();
            layers.push(Layer::crop(fitted_crop(
                crop_width,
                crop_height,
                angle,
                rect,
            )));
            layers.extend(after.iter().copied().map(turn));
            tails.push((
                format!("crop at {angle} with {before:?} before and {after:?} after"),
                layers,
                angle != 0.0,
            ));
        }
    }
    // A pixel edit, a colour layer and a spatial layer before a straightened crop: three more
    // operations and one more segment, and not one of them moves a coordinate.
    tails.push((
        "pixel, colour and spatial layers before a straightened crop".to_owned(),
        vec![
            Layer::pixel(3, 4, [250, 1, 2]),
            Layer {
                id: LayerId::new(),
                effect_id: crate::BASIC_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"exposure": 0.4, "contrast": 20.0}),
                mask: None,
                artifacts: Vec::new(),
            },
            Layer {
                id: LayerId::new(),
                effect_id: crate::PRESENCE_EFFECT.into(),
                effect_format: EFFECT_FORMAT,
                payload: json!({"texture": 35.0}),
                mask: None,
                artifacts: Vec::new(),
            },
            Layer::crop(fitted_crop(width, height, 6.0, [0.2, 0.2, 0.55, 0.55])),
        ],
        true,
    ));
    tails
}

/// `stage_transform` is `locate` in closed form, so the two must name the same
/// content pixel.
///
/// The strong direction is `inverse`: rounding the continuous content coordinate it gives for an
/// output pixel center to the pixel that contains it must be, exactly, the pixel
/// `locate` walks to. That is not the same arithmetic — `locate` rounds at
/// the resample and then applies the exact steps before it as integers, while this composes
/// everything continuously and rounds once at the end — so the agreement is a real check on the
/// half-pixel convention and on the composition order, not a re-run of the walk.
///
/// The two orders can disagree in exactly one place: a position that lands *on* a content pixel
/// boundary, where `nearest_index`'s floor takes the lower index in the resample's own frame and
/// a reflection after it turns that into the higher index in the content stage. The seeded points
/// below never land on one, and this asserts that rather than allowing a pixel of slack.
#[test]
fn the_stage_transform_and_locate_name_the_same_content_pixel() {
    let registry = registry();
    let (width, height) = (40, 28);
    let source = gradient(width, height);
    for (case, layers, _) in geometry_tails(width, height) {
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        };
        let transform = stage_transform(&registry, width, height, &recipe).unwrap();
        let (stage_width, stage_height) = extents(&registry, &source, &recipe).unwrap();
        assert_eq!(
            (transform.content.width, transform.content.height),
            (width, height),
            "{case}: the content stage"
        );
        assert_eq!(
            (transform.output.width, transform.output.height),
            (stage_width, stage_height),
            "{case}: the output stage"
        );
        let mut rng = Lcg(0x5EED_0007);
        for _ in 0..400 {
            let x = (rng.next_unit() * f64::from(stage_width)).floor() as u32;
            let y = (rng.next_unit() * f64::from(stage_height)).floor() as u32;
            let (x, y) = (x.min(stage_width - 1), y.min(stage_height - 1));
            let (u, v) = at(transform.inverse, f64::from(x) + 0.5, f64::from(y) + 0.5);
            assert!(
                u.fract() != 0.0 && v.fract() != 0.0,
                "{case}: ({x}, {y}) maps onto a content pixel boundary at ({u}, {v}), \
                 where the two rounding orders are allowed to differ"
            );
            let located =
                locate_recipe(&registry, source.width, source.height, &recipe, x, y).unwrap();
            assert_eq!(
                (located.content_x, located.content_y),
                (nearest_index(u, width), nearest_index(v, height)),
                "{case}: ({x}, {y}) maps to ({u}, {v})"
            );
            assert_eq!(
                (located.width, located.height),
                (width, height),
                "{case}: the content stage"
            );
        }
    }
}

/// The acceptance direction: project a content point through `forward` and locate the rendered
/// pixel it lands in.
///
/// For every exact tail this is exact. `forward` is then a signed permutation of continuous
/// coordinates, so it carries the pixel containing the content point onto the pixel containing
/// its image, and `locate` walks straight back to it.
///
/// A straightened crop cannot be exact in the index domain, and the reason is arithmetic rather
/// than approximate: taking the output *pixel* the projection lands in discards up to half a
/// pixel in each axis, and `inverse` carries that back into the content stage scaled by its own
/// coefficients, `½(|m0| + |m1|)` in x and `½(|m3| + |m4|)` in y — about 0.71 content pixels for
/// the rotation a crop applies. A displacement that large can cross one content pixel boundary
/// and no more, so the located pixel is within one index, and the continuous displacement is
/// asserted against that derived budget rather than against a chosen number.
#[test]
fn projecting_a_content_point_forward_locates_the_pixel_it_came_from() {
    let registry = registry();
    let (width, height) = (40, 28);
    let source = gradient(width, height);
    for (case, layers, resamples) in geometry_tails(width, height) {
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        };
        let transform = stage_transform(&registry, width, height, &recipe).unwrap();
        let budget_x = 0.5 * (transform.inverse[0].abs() + transform.inverse[1].abs());
        let budget_y = 0.5 * (transform.inverse[3].abs() + transform.inverse[4].abs());
        let mut rng = Lcg(0x5EED_0070);
        let mut located_points = 0;
        for _ in 0..1000 {
            let px = rng.next_unit() * f64::from(width);
            let py = rng.next_unit() * f64::from(height);
            let (qx, qy) = at(transform.forward, px, py);
            // A content point the crop discarded has no rendered pixel to locate.
            if qx < 0.0
                || qy < 0.0
                || qx >= f64::from(transform.output.width)
                || qy >= f64::from(transform.output.height)
            {
                continue;
            }
            located_points += 1;
            let (x, y) = (qx.floor() as u32, qy.floor() as u32);
            let located =
                locate_recipe(&registry, source.width, source.height, &recipe, x, y).unwrap();
            let (content_x, content_y) = (px.floor() as u32, py.floor() as u32);
            if !resamples {
                assert_eq!(
                    (located.content_x, located.content_y),
                    (content_x, content_y),
                    "{case}: ({px}, {py}) projects to ({qx}, {qy})"
                );
                continue;
            }
            // The centre of the pixel the projection landed in, carried back: within the budget
            // of the content point it started from, and therefore within one content pixel.
            let (u, v) = at(transform.inverse, f64::from(x) + 0.5, f64::from(y) + 0.5);
            assert!(
                (u - px).abs() <= budget_x + 1e-9 && (v - py).abs() <= budget_y + 1e-9,
                "{case}: ({px}, {py}) came back as ({u}, {v}), outside \
                 ({budget_x}, {budget_y})"
            );
            assert!(
                located.content_x.abs_diff(content_x) <= 1
                    && located.content_y.abs_diff(content_y) <= 1,
                "{case}: ({px}, {py}) located ({}, {}) instead of ({content_x}, {content_y})",
                located.content_x,
                located.content_y,
            );
        }
        // The narrowest tail here, a half-frame crop at −37.5°, keeps about a fifth of the
        // content stage; this only proves no case tested nothing.
        assert!(
            located_points > 100,
            "{case}: only {located_points} of the points landed in the output stage"
        );
    }
}

/// `forward` and `inverse` are one mapping stated twice. The tolerance is `f64` round-off over a
/// handful of multiplies at coordinates of a few thousand — the last bits of the mantissa, some
/// three orders of magnitude under the 1e-9 asserted here — and nothing else: every exact tail
/// composes integers and half-integers, and the crop's rotation is the only inexact step.
#[test]
fn the_stage_transform_and_its_inverse_are_mutual_inverses() {
    let registry = registry();
    let (width, height) = (40, 28);
    for (case, layers, _) in geometry_tails(width, height) {
        let recipe = Recipe {
            format: crate::RECIPE_FORMAT,
            layers,
            masks: Vec::new(),
            ..Recipe::default()
        };
        let transform = stage_transform(&registry, width, height, &recipe).unwrap();
        let mut rng = Lcg(0x5EED_0700);
        for _ in 0..400 {
            let px = rng.next_unit() * f64::from(width);
            let py = rng.next_unit() * f64::from(height);
            let (qx, qy) = at(transform.forward, px, py);
            let (rx, ry) = at(transform.inverse, qx, qy);
            assert!(
                (rx - px).abs() < 1e-9 && (ry - py).abs() < 1e-9,
                "{case}: ({px}, {py}) round-tripped to ({rx}, {ry})"
            );
            let ox = rng.next_unit() * f64::from(transform.output.width);
            let oy = rng.next_unit() * f64::from(transform.output.height);
            let (cx, cy) = at(transform.inverse, ox, oy);
            let (bx, by) = at(transform.forward, cx, cy);
            assert!(
                (bx - ox).abs() < 1e-9 && (by - oy).abs() < 1e-9,
                "{case}: output ({ox}, {oy}) round-tripped to ({bx}, {by})"
            );
        }
    }
}

/// A stack with no output stage has no mapping, and says so in the voice the neighbouring
/// methods use. An identity matrix would put a gesture's pointer where the recipe never puts it,
/// so no path here returns one.
#[test]
fn a_stack_without_a_renderable_output_stage_has_no_transform() {
    let registry = geometry_registry();
    let (width, height) = (32, 24);

    // An effect no module provides: `extents` and `locate` report it this way too.
    let recipe = Recipe {
        format: crate::RECIPE_FORMAT,
        layers: vec![Layer {
            id: LayerId::new(),
            effect_id: "test.absent".into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({}),
            mask: None,
            artifacts: Vec::new(),
        }],
        masks: Vec::new(),
        ..Recipe::default()
    };
    let error = stage_transform(&registry, width, height, &recipe)
        .expect_err("an unavailable effect has no output stage");
    assert_eq!(error.kind, ErrorKind::Incompatible);
    assert!(error.detail.contains("unavailable effect"), "{error}");

    // A finite, non-empty resample that still collapses its stage onto a line. The compiler's
    // own checks pass it, so this is the one degenerate mapping that reaches the composition.
    let recipe = Recipe {
        format: crate::RECIPE_FORMAT,
        layers: vec![degenerate_layer()],
        masks: Vec::new(),
        ..Recipe::default()
    };
    let error = stage_transform(&registry, width, height, &recipe)
        .expect_err("a collapsed stage has no mapping");
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(error.detail.contains("cannot be inverted"), "{error}");
}
