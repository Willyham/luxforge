//! The CPU proxy's sources, cache and stage against independent references.

use super::*;
use crate::{
    BASIC_EFFECT, BoxRect, CROP_EFFECT, CropStage, EFFECT_FORMAT, ErrorKind, Layer, LayerId,
    LinearImage, LinearSettings, Mask, ModuleRegistry, Orientation, PIXEL_EFFECT, PreviewSource,
    ProxyBounds, ProxyPlan, ProxyWindow, RECIPE_FORMAT, Recipe, SnapshotId, SourceImage, Stage,
    colour::srgb::decode_u8, proxy::BoxDownscale,
};
use luxforge_reference::srgb;
use serde_json::json;

// -----------------------------------------------------------------------------------------
// Independent references
// -----------------------------------------------------------------------------------------

/// The linear value the render path's own f32 table holds for one code: the f64 transfer
/// function, stored as f32. Decoding through the table is what production does, so the
/// reference has to start from the same value to be a reference and not a second algorithm.
fn decoded(code: u8) -> f64 {
    f64::from(decode_u8(code) as f32)
}

/// The forward sRGB transfer function rounded to a code. This is the definition the render
/// path's threshold table encodes; computing it directly here keeps the reference independent
/// of that table.
fn encoded(linear: f64) -> u8 {
    (srgb::encode_clamped(linear) * 255.0).round() as u8
}

fn jpeg_source(width: u32, height: u32, pixels: &[[u8; 3]]) -> PreviewSource {
    assert_eq!(pixels.len() as u64, u64::from(width) * u64::from(height));
    let mut rgba = Vec::with_capacity(pixels.len() * 4);
    for pixel in pixels {
        rgba.extend_from_slice(pixel);
        rgba.push(255);
    }
    PreviewSource::Jpeg(SourceImage {
        width,
        height,
        rgba: rgba.into(),
        fingerprint: "sha256:proxy-fixture".into(),
        orientation: 1,
        capture: Default::default(),
    })
}

fn raw_source(width: u32, height: u32, pixels: &[[f32; 3]]) -> LinearImage {
    assert_eq!(pixels.len() as u64, u64::from(width) * u64::from(height));
    let mut planes = Vec::with_capacity(pixels.len() * 3);
    for channel in 0..3 {
        planes.extend(pixels.iter().map(|pixel| pixel[channel]));
    }
    LinearImage::with_fingerprint(width, height, planes, "sha256:proxy-raw").expect("an image")
}

fn recipe(layers: Vec<Layer>) -> Recipe {
    Recipe {
        format: RECIPE_FORMAT,
        layers,
        masks: Vec::new(),
        ..Recipe::default()
    }
}

fn basic_layer(payload: serde_json::Value) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: BASIC_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload,
        mask: None,
        artifacts: Vec::new(),
    }
}

fn presence_layer(payload: serde_json::Value) -> Layer {
    Layer {
        id: LayerId::new(),
        effect_id: crate::PRESENCE_EFFECT.into(),
        effect_format: EFFECT_FORMAT,
        payload,
        mask: None,
        artifacts: Vec::new(),
    }
}

/// The payload a crop draft would commit: the whole rotated box fitted onto the stage and
/// normalized, which is a rectangle the crop contract accepts at any angle.
fn fitted_crop_layer(width: u32, height: u32, angle: f64) -> Layer {
    let stage = CropStage {
        width,
        height,
        angle,
    };
    let (box_width, box_height) = stage.bounding_box();
    let fitted = stage.fit_about_center(BoxRect {
        x: 0.0,
        y: 0.0,
        width: box_width,
        height: box_height,
    });
    Layer::crop(fitted.normalized(&stage))
}

fn plan(width: u32, height: u32, bounds: (u32, u32)) -> ProxyPlan {
    ProxyPlan {
        width,
        height,
        bounds: ProxyBounds {
            width: bounds.0,
            height: bounds.1,
        },
        window: None,
    }
}

fn jpeg_of(source: &PreviewSource) -> &SourceImage {
    match source {
        PreviewSource::Jpeg(image) => image,
        PreviewSource::Raw { .. } => panic!("a JPEG proxy stays a JPEG source"),
    }
}

fn raw_of(source: &PreviewSource) -> &LinearImage {
    match source {
        PreviewSource::Raw { image, .. } => image,
        PreviewSource::Jpeg(_) => panic!("a RAW proxy stays a RAW source"),
    }
}

/// A CPU proxy holds its whole stage. Fitting a tight crop's output to the display raises the
/// scale towards one, so the whole stage is lowered to the display bounds' 8 MP, keeping the
/// source's aspect; a stage already inside them is kept as fitted, its window dropped.
#[test]
fn a_whole_stage_proxy_stays_within_the_display_bounds_pixels() {
    let source = (8256, 5504);
    let bounds = ProxyBounds {
        width: 1716,
        height: 1576,
    };
    // A 25% crop of a 45 MP source fitted to the bounds: a 0.42 scale, 8 MP and more.
    let tight = ProxyPlan::fit(source, (2064, 1376), bounds).expect("a reduced stage");
    assert!(u64::from(tight.width) * u64::from(tight.height) > ProxyBounds::MAX_PIXELS);
    let whole = tight.whole_within(source);
    assert!(u64::from(whole.width) * u64::from(whole.height) <= ProxyBounds::MAX_PIXELS);
    assert!(whole.width < tight.width && whole.height < tight.height);
    let aspect = |width: u32, height: u32| f64::from(width) / f64::from(height);
    assert!((aspect(whole.width, whole.height) - aspect(source.0, source.1)).abs() < 1e-3);
    assert_eq!((whole.bounds, whole.window), (bounds, None));
    // A whole stack fitted to the same bounds is already display-sized.
    let fitted = ProxyPlan {
        window: Some(ProxyWindow {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        }),
        ..ProxyPlan::fit(source, source, bounds).expect("a reduced stage")
    };
    assert_eq!(fitted.whole_within(source), fitted.whole());
}

/// An integer-scale downscale equals, per output pixel, the mean of its block's decoded linear
/// values re-quantized at the code boundary — computed here in f64 from the transfer function
/// itself rather than from the renderer's threshold table.
#[test]
fn an_integer_scale_downscale_is_the_exact_mean_of_each_block() {
    let codes: Vec<[u8; 3]> = (0..48u32)
        .map(|index| {
            [
                (index * 5 + 3) as u8,
                (index * 11 + 17) as u8,
                (255 - index * 3) as u8,
            ]
        })
        .collect();
    let source = jpeg_source(8, 6, &codes);
    for (width, height) in [(4u32, 3u32), (2, 3), (1, 1)] {
        let block_width = 8 / width as usize;
        let block_height = 6 / height as usize;
        let proxy = source
            .proxy(plan(width, height, (width, height)))
            .expect("a proxy");
        let image = jpeg_of(&proxy);
        assert_eq!((image.width, image.height), (width, height));
        assert_eq!(image.fingerprint, "sha256:proxy-fixture");
        assert_eq!(image.orientation, 1);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let mut sum = [0f64; 3];
                for row in 0..block_height {
                    for column in 0..block_width {
                        let code = codes[(y * block_height + row) * 8 + x * block_width + column];
                        for (channel, value) in sum.iter_mut().enumerate() {
                            *value += decoded(code[channel]);
                        }
                    }
                }
                let count = (block_width * block_height) as f64;
                let expected = [
                    encoded(sum[0] / count),
                    encoded(sum[1] / count),
                    encoded(sum[2] / count),
                ];
                let at = (y * width as usize + x) * 4;
                assert_eq!(
                    [image.rgba[at], image.rgba[at + 1], image.rgba[at + 2]],
                    expected,
                    "{width}x{height} block ({x}, {y})"
                );
                assert_eq!(image.rgba[at + 3], 255, "alpha is always opaque");
            }
        }
    }
}

/// A uniform image stays exactly uniform at a fractional scale, and a horizontal gradient stays
/// monotone across it.
#[test]
fn a_fractional_scale_preserves_uniformity_and_monotonicity() {
    let uniform: Vec<[u8; 3]> = vec![[97, 13, 200]; 35];
    let proxy = jpeg_source(7, 5, &uniform)
        .proxy(plan(3, 2, (3, 2)))
        .expect("a proxy");
    let image = jpeg_of(&proxy);
    assert_eq!((image.width, image.height), (3, 2));
    for pixel in image.rgba.chunks_exact(4) {
        assert_eq!(pixel, [97, 13, 200, 255]);
    }

    let gradient: Vec<[u8; 3]> = (0..35)
        .map(|index| {
            let code = ((index % 7) * 36) as u8;
            [code, code, code]
        })
        .collect();
    let proxy = jpeg_source(7, 5, &gradient)
        .proxy(plan(3, 2, (3, 2)))
        .expect("a proxy");
    let image = jpeg_of(&proxy);
    for y in 0..2usize {
        let row: Vec<u8> = (0..3).map(|x| image.rgba[(y * 3 + x) * 4]).collect();
        assert!(
            row.windows(2).all(|pair| pair[0] < pair[1]),
            "a horizontal gradient stays monotone: {row:?}"
        );
    }
}

#[test]
fn a_uniform_image_is_unchanged_at_any_scale_for_both_source_kinds() {
    let jpeg = jpeg_source(12, 9, &vec![[31, 199, 4]; 108]);
    for (width, height) in [(1u32, 1u32), (2, 3), (5, 4), (11, 8), (12, 9)] {
        let image = jpeg_of(&jpeg.proxy(plan(width, height, (width, height))).unwrap()).clone();
        assert_eq!((image.width, image.height), (width, height));
        for pixel in image.rgba.chunks_exact(4) {
            assert_eq!(pixel, [31, 199, 4, 255], "{width}x{height} is not uniform");
        }
    }

    let raw = PreviewSource::Raw {
        image: raw_source(12, 9, &vec![[0.25, 1.75, -0.5]; 108]),
        settings: LinearSettings::default(),
    };
    for (width, height) in [(1u32, 1u32), (2, 3), (5, 4), (11, 8), (12, 9)] {
        let proxy = raw.proxy(plan(width, height, (width, height))).unwrap();
        let image = raw_of(&proxy);
        assert_eq!((image.width(), image.height()), (width, height));
        for y in 0..height {
            for x in 0..width {
                assert_eq!(
                    image.pixel(x, y),
                    Some([0.25, 1.75, -0.5]),
                    "{width}x{height} at ({x}, {y})"
                );
            }
        }
    }
}

/// The proxy of a RAW source reads through its view: a crop and an EXIF orientation are
/// resolved by the averaging, so the result is upright content with an identity view, the same
/// fingerprint, and values equal to the mean of the viewed planes.
#[test]
fn a_raw_proxy_averages_the_viewed_planes_and_keeps_the_fingerprint() {
    let pixels: Vec<[f32; 3]> = (0..64)
        .map(|index| {
            let value = index as f32;
            [value, value * 0.5 - 3.0, 100.0 - value]
        })
        .collect();
    let base = raw_source(8, 8, &pixels);
    // A 6x4 crop at (1, 1), read with EXIF orientation 6: a quarter turn, so the viewed image
    // is 4 wide and 6 tall.
    let viewed = base.with_view([1, 1, 6, 4], 6).expect("a view");
    assert_eq!((viewed.width(), viewed.height()), (4, 6));
    let source = PreviewSource::Raw {
        image: viewed.clone(),
        settings: LinearSettings::default(),
    };

    let proxy = source.proxy(plan(2, 3, (2, 3))).expect("a proxy");
    let image = raw_of(&proxy);
    assert_eq!((image.width(), image.height()), (2, 3));
    assert_eq!(image.fingerprint(), "sha256:proxy-raw");
    assert_eq!(
        image.view(),
        ([0, 0, 2, 3], 1),
        "a proxy has an identity view"
    );

    for y in 0..3u32 {
        for x in 0..2u32 {
            let mut sum = [0f64; 3];
            for row in 0..2u32 {
                for column in 0..2u32 {
                    let pixel = viewed
                        .pixel(x * 2 + column, y * 2 + row)
                        .expect("a viewed pixel");
                    for (channel, value) in sum.iter_mut().enumerate() {
                        *value += f64::from(pixel[channel]);
                    }
                }
            }
            let actual = image.pixel(x, y).expect("a proxy pixel");
            for channel in 0..3 {
                let expected = (sum[channel] / 4.0) as f32;
                assert!(
                    (actual[channel] - expected).abs() <= f32::EPSILON * expected.abs().max(1.0),
                    "({x}, {y}) channel {channel}: {} against {expected}",
                    actual[channel]
                );
            }
        }
    }
}

/// A RAW proxy is a weighted mean of a source whose values were all finite when it was built,
/// so it is adopted through the validated constructor and skips the scan of every value that
/// the public constructors make on untrusted input. Its planes, fingerprint, view and
/// development are exactly what `with_fingerprint` makes of the same planes (which does scan),
/// including at the largest finite values, whose mean must not overflow.
#[test]
fn a_raw_proxy_of_a_finite_source_skips_the_finiteness_scan_and_equals_the_scanned_one() {
    use crate::source::finiteness_scans_during;
    let (width, height) = (53u32, 41u32);
    let ordinary: Vec<[f32; 3]> = (0..width * height)
        .map(|index| {
            let value = index as f32;
            [
                (value * 0.031) % 1.4 - 0.1,
                (value * 0.047) % 1.2 - 3.0,
                1.0e6 - value,
            ]
        })
        .collect();
    let extreme: Vec<[f32; 3]> = (0..width * height)
        .map(|index| match index % 3 {
            0 => [f32::MAX, -f32::MAX, f32::MIN_POSITIVE],
            1 => [f32::MAX, -f32::MAX, 0.0],
            _ => [f32::MAX, -f32::MAX, -0.0],
        })
        .collect();
    for pixels in [ordinary, extreme] {
        let base = raw_source(width, height, &pixels)
            .with_view([2, 1, 49, 38], 6)
            .expect("a view");
        let source = PreviewSource::Raw {
            image: base,
            settings: LinearSettings::default(),
        };
        let (source_width, source_height) = source.dimensions();
        for plan in [
            plan(source_width * 3 / 7, source_height * 5 / 9, (1, 1)),
            plan(source_width, source_height, (1, 1)),
            plan(1, 1, (1, 1)),
        ] {
            let (proxy, scans) = finiteness_scans_during(|| source.proxy(plan));
            let proxy = proxy.expect("a proxy");
            assert_eq!(scans, 0, "{}x{} proxy", plan.width, plan.height);
            let image = raw_of(&proxy);

            let (reference, scans) = finiteness_scans_during(|| {
                LinearImage::with_fingerprint(
                    plan.width,
                    plan.height,
                    image.planes().to_vec(),
                    image.fingerprint(),
                )
            });
            assert_eq!(scans, 1, "the public constructor scans the same planes");
            let reference = reference
                .expect("the planes are finite")
                .with_development(image.development());
            assert_eq!(image, &reference);
            assert_eq!(image.planes().len(), reference.planes().len());
            assert!(
                image
                    .planes()
                    .iter()
                    .zip(reference.planes())
                    .all(|(a, b)| a.to_bits() == b.to_bits() && a.is_finite()),
                "{}x{} proxy planes",
                plan.width,
                plan.height
            );
        }
    }

    // The public constructors still refuse what the proxy path never produces.
    for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut planes = vec![0.5_f32; 12];
        planes[7] = bad;
        let (image, scans) = finiteness_scans_during(|| LinearImage::new(2, 2, planes.clone()));
        assert!(image.is_err(), "{bad} is refused by `new`");
        assert_eq!(scans, 1);
        assert!(LinearImage::with_fingerprint(2, 2, planes, "sha256:bad").is_err());
    }
}

/// The white-balance approximation is one matrix per pixel and the downscale an area average,
/// both linear, so they commute: the proxy of planes the matrix was applied to equals the
/// matrix applied to the proxy of the planes, to f32 rounding. That is why an approximate
/// job's proxy phase describes the same approximation its full-size phase does, at display
/// size, through the same filter as every other proxy.
#[test]
fn a_white_balance_approximation_commutes_with_the_downscale() {
    let matrix = [[1.31, 0.07, -0.03], [0.02, 0.96, 0.05], [-0.08, 0.03, 0.69]];
    let (width, height) = (37, 23);
    let pixels: Vec<[f32; 3]> = (0..width * height)
        .map(|index| {
            let value = index as f32;
            [
                (value * 0.031) % 1.4 - 0.1,
                (value * 0.047) % 1.2,
                (value * 0.019) % 1.7,
            ]
        })
        .collect();
    let balanced: Vec<[f32; 3]> = pixels
        .iter()
        .map(|pixel| {
            matrix.map(|row| {
                (row[0] * f64::from(pixel[0])
                    + row[1] * f64::from(pixel[1])
                    + row[2] * f64::from(pixel[2])) as f32
            })
        })
        .collect();
    let settings = LinearSettings::default();
    let proxy_of = |pixels: &[[f32; 3]]| {
        PreviewSource::Raw {
            image: raw_source(width, height, pixels),
            settings,
        }
        .proxy(plan(11, 7, (11, 7)))
        .expect("a proxy")
    };
    let downscaled = proxy_of(&pixels);
    let balanced_then_downscaled = proxy_of(&balanced);
    for y in 0..7 {
        for x in 0..11 {
            let proxy = raw_of(&downscaled).pixel(x, y).unwrap().map(f64::from);
            let expected = raw_of(&balanced_then_downscaled).pixel(x, y).unwrap();
            let balanced_proxy =
                matrix.map(|row| row[0] * proxy[0] + row[1] * proxy[1] + row[2] * proxy[2]);
            for channel in 0..3 {
                let expected = f64::from(expected[channel]);
                assert!(
                    (balanced_proxy[channel] - expected).abs() <= 1.0e-5 * expected.abs().max(1.0),
                    "({x}, {y}) channel {channel}: {} against {expected}",
                    balanced_proxy[channel]
                );
            }
        }
    }
    // And so do the frames the two render: the approximation over the proxy, against the
    // matrix applied before the downscale, agree to one code at most — f32 rounding at a code
    // boundary, never a visible difference.
    let registry = ModuleRegistry::builtin();
    let approximate = PreviewSource::Raw {
        image: raw_of(&downscaled).clone(),
        settings: LinearSettings {
            white_balance: Some(crate::WhiteBalanceApproximation::from_matrix(matrix).unwrap()),
        },
    };
    let over_proxy = approximate
        .render(&registry, SnapshotId::new(), &recipe(Vec::new()))
        .unwrap();
    let before = balanced_then_downscaled
        .render(&registry, SnapshotId::new(), &recipe(Vec::new()))
        .unwrap();
    assert_eq!(over_proxy.rgba.len(), before.rgba.len());
    for (a, b) in over_proxy.rgba.iter().zip(before.rgba.iter()) {
        assert!(a.abs_diff(*b) <= 1, "{a} against {b}");
    }
}

#[test]
fn a_pixel_stage_layer_makes_a_stack_ineligible_and_is_named() {
    let registry = ModuleRegistry::developer();
    let eligible = recipe(vec![
        Layer::orientation(Orientation {
            mirror: true,
            turns: 3,
        }),
        basic_layer(json!({ "exposure": 0.5 })),
        fitted_crop_layer(480, 320, 4.0),
    ]);
    registry
        .proxy_eligible(&eligible)
        .expect("orientation, Basic and crop are all resolution independent");

    let ineligible = recipe(vec![
        Layer::orientation(Orientation::NEUTRAL),
        Layer::pixel(3, 4, [9, 9, 9]),
        fitted_crop_layer(480, 320, 0.0),
    ]);
    let error = registry
        .proxy_eligible(&ineligible)
        .expect_err("a point replacement addresses content pixels");
    assert_eq!(error.kind, ErrorKind::Validation);
    assert!(error.detail.contains(PIXEL_EFFECT), "{error}");
    assert!(error.detail.contains("layer 1"), "{error}");

    let unknown = recipe(vec![Layer {
        id: LayerId::new(),
        effect_id: "test.absent".into(),
        effect_format: EFFECT_FORMAT,
        payload: json!({}),
        mask: None,
        artifacts: Vec::new(),
    }]);
    let error = registry
        .proxy_eligible(&unknown)
        .expect_err("an unknown effect has no stage");
    assert!(error.detail.contains("test.absent"), "{error}");
    assert!(error.detail.contains("layer 0"), "{error}");
}

/// A finish-stage layer is exact at proxy scale (its mask is normalized to the output stage)
/// and a spatial-stage layer is eligible but approximate, which the registry says separately.
#[test]
fn finish_layers_are_exact_and_spatial_layers_are_approximate_at_proxy_scale() {
    let registry = ModuleRegistry::builtin();
    let finish = recipe(vec![
        basic_layer(json!({ "exposure": 0.5 })),
        Layer {
            id: LayerId::new(),
            effect_id: crate::VIGNETTE_EFFECT.into(),
            effect_format: EFFECT_FORMAT,
            payload: json!({ "amount": -40 }),
            mask: None,
            artifacts: Vec::new(),
        },
    ]);
    registry
        .proxy_eligible(&finish)
        .expect("a vignette is resolution independent");
    assert!(!registry.proxy_approximation(&finish, 96, 64).spatial);

    let spatial = recipe(vec![
        basic_layer(json!({ "exposure": 0.5 })),
        presence_layer(json!({ "clarity": 60 })),
    ]);
    registry
        .proxy_eligible(&spatial)
        .expect("a Presence stack renders through the proxy");
    assert!(
        registry.proxy_approximation(&spatial, 96, 64).spatial,
        "its neighbourhoods scale with the stage, so the proxy frame is approximate"
    );
}

/// A reset Presence layer is still a spatial-stage layer, but its neutral payload compiles to
/// no operation at all: the proxy frame is the exact recipe at proxy size, byte for byte with
/// the exact recipe over the exact downscale, and is not labelled approximate.
#[test]
fn a_neutral_spatial_layer_is_not_approximate_at_proxy_scale() {
    let registry = ModuleRegistry::builtin();
    for payload in [
        json!({}),
        json!({ "texture": 0, "clarity": 0, "dehaze": 0 }),
    ] {
        let reset = recipe(vec![
            basic_layer(json!({ "exposure": 0.5 })),
            presence_layer(payload.clone()),
        ]);
        registry
            .proxy_eligible(&reset)
            .expect("a Presence stack renders through the proxy");
        let approximation = registry.proxy_approximation(&reset, 96, 64);
        assert!(!approximation.spatial, "{payload}");
        assert!(!approximation.is_approximate(), "{payload}");
        assert_eq!(approximation.reason(), None, "{payload}");

        // And the frame is what the label says: the stack without its neutral layer.
        let pixels: Vec<[u8; 3]> = (0..96_u32 * 64)
            .map(|index| {
                [
                    (index % 251) as u8,
                    (index * 7 % 253) as u8,
                    (index * 13 % 241) as u8,
                ]
            })
            .collect();
        let source = jpeg_source(96, 64, &pixels);
        let bounds = ProxyBounds {
            width: 48,
            height: 32,
        };
        let plan = source
            .proxy_plan(&registry, &reset, bounds)
            .unwrap()
            .expect("a proxy is worthwhile");
        let proxy = source.proxy(plan).unwrap();
        let without = recipe(vec![basic_layer(json!({ "exposure": 0.5 }))]);
        assert_eq!(
            proxy
                .render(&registry, SnapshotId::new(), &reset)
                .unwrap()
                .rgba,
            proxy
                .render(&registry, SnapshotId::new(), &without)
                .unwrap()
                .rgba,
            "{payload}: a neutral spatial layer changes no proxy byte"
        );
    }
}

/// The cache holds one proxy under one key: the same key hits and builds nothing, and a
/// resized window, a different plan or a different source is a miss. A miss releases the held
/// proxy's pixels before its replacement is built, so the two never coexist, and the cache
/// then holds the replacement alone. A build that is cancelled leaves the cache empty, which
/// the next job reads as a miss and rebuilds.
#[test]
fn the_cache_holds_one_entry_and_releases_it_before_building_its_replacement() {
    let source = jpeg_source(8, 6, &[[40, 80, 120]; 48]);
    let other = jpeg_source(8, 6, &[[40, 80, 120]; 48]);
    let other = PreviewSource::Jpeg(SourceImage {
        fingerprint: "sha256:other".into(),
        ..jpeg_of(&other).clone()
    });
    let key = |source: &PreviewSource, plan: ProxyPlan| ProxyKey {
        identity: source.identity(),
        plan,
    };
    let first = key(&source, plan(4, 3, (4, 3)));
    let mut cache = ProxyCache::default();
    let (built, fresh) = cache
        .source_for(&first, &source, || source.proxy(first.plan))
        .expect("a proxy");
    assert!(fresh, "an empty cache never hits");
    let (hit, fresh) = cache
        .source_for(&first, &source, || panic!("a hit builds nothing"))
        .expect("the held proxy");
    assert!(!fresh, "the same key hits");
    assert!(std::sync::Arc::ptr_eq(
        &jpeg_of(&hit).rgba,
        &jpeg_of(&built).rgba
    ));
    drop((built, hit));

    let misses = [
        // A resized window is a miss even at the same rounded dimensions.
        (key(&source, plan(4, 3, (5, 3))), &source),
        // A different plan is a miss.
        (key(&source, plan(2, 3, (4, 3))), &source),
        // A different source identity is a miss.
        (key(&other, first.plan), &other),
    ];
    for (miss, miss_source) in misses {
        let (held, _) = cache
            .source_for(&first, &source, || source.proxy(first.plan))
            .expect("the first proxy");
        let pixels = std::sync::Arc::downgrade(&jpeg_of(&held).rgba);
        drop(held);
        let (replacement, fresh) = cache
            .source_for(&miss, miss_source, || {
                assert!(
                    pixels.upgrade().is_none(),
                    "the held proxy is released before its replacement is built"
                );
                miss_source.proxy(miss.plan)
            })
            .expect("the replacement");
        assert!(fresh, "{:?} is a miss", miss.plan);
        let (_, fresh) = cache
            .source_for(&miss, miss_source, || panic!("the replacement is held"))
            .expect("the held replacement");
        assert!(!fresh);
        drop(replacement);
    }
    let (_, fresh) = cache
        .source_for(&first, &source, || source.proxy(first.plan))
        .expect("the first proxy");
    assert!(fresh, "the cache held one entry, the last replacement");

    let cancelled = Cancel::new();
    cancelled.cancel();
    let second = key(&source, plan(2, 3, (2, 3)));
    let refused = cache.source_for(&second, &source, || {
        source.proxy_cancellable(second.plan, &cancelled)
    });
    assert_eq!(
        refused.err().map(|error| error.kind),
        Some(ErrorKind::Cancelled)
    );
    let (_, fresh) = cache
        .source_for(&first, &source, || source.proxy(first.plan))
        .expect("the first proxy");
    assert!(
        fresh,
        "a cancelled build leaves the cache empty, so the next job rebuilds"
    );
}

/// A JPEG proxy's pixels are written in the allocation the proxy source holds, with no copy
/// after the pass, and an identity stack rendered over it returns that allocation itself.
#[test]
fn a_jpeg_proxy_holds_the_frame_its_pass_wrote() {
    let codes: Vec<[u8; 3]> = (0..48u32)
        .map(|index| [(index * 5) as u8, (index * 3 + 7) as u8, 200])
        .collect();
    let source = jpeg_source(8, 6, &codes);
    let (proxy, written) =
        crate::render::frame_writes::record(|| source.proxy(plan(4, 3, (4, 3))).expect("a proxy"));
    let image = jpeg_of(&proxy);
    assert_eq!(written, [image.rgba.as_ptr() as usize]);
    let frame = proxy
        .render_proxy_cancellable(
            &ModuleRegistry::builtin(),
            SnapshotId::new(),
            &recipe(Vec::new()),
            &Cancel::never(),
        )
        .expect("a frame");
    assert!(std::sync::Arc::ptr_eq(&frame.rgba, &image.rgba));
}

/// A RAW identity follows the developed planes: redeveloping them misses, and a view change
/// misses, while the same planes under the same view hit.
#[test]
fn a_raw_identity_follows_its_developed_planes() {
    let pixels: Vec<[f32; 3]> = (0..64).map(|index| [index as f32; 3]).collect();
    let image = raw_source(8, 8, &pixels);
    let source = PreviewSource::Raw {
        image: image.clone(),
        settings: LinearSettings::default(),
    };
    let same = PreviewSource::Raw {
        image: image.clone(),
        settings: LinearSettings::default(),
    };
    assert_eq!(
        source.identity(),
        same.identity(),
        "a shared plane allocation is the same source"
    );

    let viewed = PreviewSource::Raw {
        image: image.with_view([0, 0, 4, 4], 1).expect("a view"),
        settings: LinearSettings::default(),
    };
    assert_ne!(
        source.identity(),
        viewed.identity(),
        "a view change is a different source"
    );

    let redeveloped = PreviewSource::Raw {
        image: raw_source(8, 8, &pixels),
        settings: LinearSettings::default(),
    };
    assert_ne!(
        source.identity(),
        redeveloped.identity(),
        "redeveloped planes are a different source"
    );
}

/// The whole effective recipe — orientation, a Basic colour layer and a straightened crop —
/// renders against the proxy source through the existing compiled path, produces an output
/// that fits the bounds, and agrees with the point sampler over that same proxy, which is the
/// contract every point query in the host depends on.
#[test]
fn a_full_stack_renders_against_the_proxy_and_agrees_with_the_sampler() {
    let registry = ModuleRegistry::builtin();
    let pixels: Vec<[u8; 3]> = (0..64 * 48)
        .map(|index| {
            [
                (index % 251) as u8,
                ((index * 7) % 241) as u8,
                ((index * 13) % 239) as u8,
            ]
        })
        .collect();
    let source = jpeg_source(64, 48, &pixels);
    let stack = recipe(vec![
        basic_layer(json!({ "exposure": 0.5, "contrast": 20 })),
        Layer::orientation(Orientation {
            mirror: false,
            turns: 1,
        }),
        fitted_crop_layer(48, 64, 7.0),
    ]);
    let bounds = ProxyBounds {
        width: 20,
        height: 20,
    };
    let plan = source
        .proxy_plan(&registry, &stack, bounds)
        .expect("a plan")
        .expect("a proxy is worthwhile");
    assert!(plan.width < 64 && plan.height < 48, "{plan:?}");

    let proxy = source.proxy(plan).expect("a proxy source");
    assert_eq!(proxy.dimensions(), (plan.width, plan.height));
    assert_eq!(proxy.fingerprint(), source.fingerprint());

    let raster = proxy
        .render(&registry, SnapshotId::new(), &stack)
        .expect("the stack renders at proxy size");
    assert!(
        raster.width <= bounds.width && raster.height <= bounds.height,
        "{}x{} does not fit the bounds",
        raster.width,
        raster.height
    );
    assert!(raster.width > 0 && raster.height > 0);

    for (x, y) in [
        (0, 0),
        (raster.width - 1, 0),
        (0, raster.height - 1),
        (raster.width - 1, raster.height - 1),
        (raster.width / 2, raster.height / 3),
    ] {
        let sample = proxy.sample(&registry, &stack, x, y).expect("a sample");
        assert_eq!((sample.width, sample.height), (raster.width, raster.height));
        assert_eq!(
            sample.rgba,
            raster.pixel(x, y),
            "the sampler disagrees with the rendered proxy at ({x}, {y})"
        );
    }
    // The stack the proxy renders is the stack the registry called eligible.
    registry.proxy_eligible(&stack).expect("an eligible stack");
    assert_eq!(raster.source_fingerprint, source.fingerprint());
    // The crop layer is the last one, and CROP_EFFECT is what the fitted layer carries.
    assert_eq!(stack.layers[2].effect_id, CROP_EFFECT);
}

/// The proxy build on the generated 24 MP and 60 MP sources, which are the only inputs that
/// say anything about cost. It is ignored by default because it needs the generated fixtures
/// and because timing gates do not belong in CI. For the peak memory of one size, set
/// `LUXFORGE_PROXY_BUILD_SOURCE` to that fixture's path and wrap the run in `/usr/bin/time -l`;
/// `LUXFORGE_PROXY_BUILDS=0` decodes the source and builds nothing, which is the floor the
/// build's own peak is read against:
///
/// ```text
/// cargo run --release --locked --package xtask -- generate-fixtures --output fixtures/generated
/// LUXFORGE_PROXY_BUILD_SOURCE=fixtures/generated/60mp.jpg /usr/bin/time -l \
///   cargo test --release --package luxforge-core --lib measure_the_proxy_build -- --ignored --nocapture
/// ```
///
/// Each line ends with a hash of the last proxy's bytes, so two builds of the code can be
/// compared for identical output on photo-sized sources as well as for cost.
#[test]
#[ignore = "needs fixtures/generated and is a measurement, not a gate"]
fn measure_the_proxy_build_on_photo_sized_sources() {
    use std::hash::{Hash, Hasher};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let sources: Vec<std::path::PathBuf> = match std::env::var("LUXFORGE_PROXY_BUILD_SOURCE") {
        Ok(path) => vec![root.join(path)],
        Err(_) => ["24mp.jpg", "60mp.jpg"]
            .iter()
            .map(|name| root.join("fixtures/generated").join(name))
            .collect(),
    };
    let builds = std::env::var("LUXFORGE_PROXY_BUILDS")
        .map_or(15, |count| count.parse().expect("a build count"));
    let registry = ModuleRegistry::builtin();
    let stack = recipe(vec![basic_layer(
        json!({ "exposure": 0.5, "contrast": 20 }),
    )]);
    let bounds = ProxyBounds {
        width: 2880,
        height: 1800,
    };
    for path in sources {
        let source = PreviewSource::Jpeg(
            crate::open_source(&path).expect("a generated fixture: run generate-fixtures"),
        );
        let plan = source
            .proxy_plan(&registry, &stack, bounds)
            .expect("a plan")
            .expect("a photo-sized source needs a proxy at this size");
        let mut samples = Vec::new();
        let mut hash = None;
        for _ in 0..builds {
            let start = std::time::Instant::now();
            let proxy = source.proxy(plan).expect("a proxy");
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(proxy.dimensions(), (plan.width, plan.height));
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            jpeg_of(&proxy).rgba.hash(&mut hasher);
            hash = Some(hasher.finish());
        }
        let (width, height) = source.dimensions();
        let Some(cold) = samples.first().copied() else {
            println!(
                "{} {width}x{height}: decoded, no proxy built",
                path.display()
            );
            continue;
        };
        // The first build carries the Rayon pool's first use and the first touch of fresh
        // buffers, which is what the first job after a window resize actually pays; the rest
        // is the steady-state cost of rebuilding one.
        let ms = luxforge_testbase::Distribution::of(samples).expect("builds ran");
        println!(
            "{} proxy build {width}x{height} -> {}x{}: cold {cold:.2} ms, warm p50 {:.2} ms, \
             p95 {:.2} ms, min {:.2} ms, max {:.2} ms, {builds} builds, bytes {:016x}",
            path.display(),
            plan.width,
            plan.height,
            ms.p50,
            ms.p95,
            ms.min,
            ms.max,
            hash.unwrap_or_default()
        );
    }
}

/// What a **mask** costs the proxy phase on photo-sized sources: the render a drag presents,
/// unmasked, masked and point sampled, and masked under the thin-feature rule, at the display
/// bounds the owner's screen offers. Ignored by default for the same two reasons as the build
/// measurement above:
///
/// ```text
/// cargo run --release --locked --package xtask -- generate-fixtures --output fixtures/generated
/// cargo test --release --package luxforge-core --lib proxy:: -- --ignored --nocapture
/// ```
#[test]
#[ignore = "needs fixtures/generated and is a measurement, not a gate"]
fn measure_the_masked_proxy_render_on_photo_sized_sources() {
    for fixture in ["24mp.jpg", "60mp.jpg"] {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/generated")
            .join(fixture);
        let source = PreviewSource::Jpeg(crate::open_source(&path).expect("a generated fixture"));
        let registry = ModuleRegistry::builtin();
        let bounds = ProxyBounds {
            width: 2880,
            height: 1800,
        };
        // Every Basic field non-neutral, so each frame runs the module's whole colour chain —
        // the stack the latency target is stated over.
        let payload = json!({
            "exposure": 0.5, "contrast": 25.0, "highlights": -30.0, "shadows": 30.0,
            "whites": -15.0, "blacks": 15.0, "temperature": 20.0, "tint": -10.0,
            "vibrance": 30.0, "saturation": 15.0,
        });
        let unmasked = recipe(vec![basic_layer(payload.clone())]);
        // A gradient over the middle half of the frame: a mask a person would draw, whose ramp
        // is hundreds of proxy pixels wide, so it is point sampled.
        let broad = gradient_mask(0.5);
        // A gradient whose ramp is a thousandth of the frame height: about 1.8 px at a 1800 px
        // proxy, which is what trips the 2 x 2 supersample of the mask field.
        let thin = gradient_mask(0.001);
        let masked_with = |mask: &Mask| Recipe {
            format: RECIPE_FORMAT,
            layers: vec![Layer {
                mask: Some(mask.id.clone()),
                ..basic_layer(payload.clone())
            }],
            masks: vec![mask.clone()],
            ..Recipe::default()
        };
        let broad_stack = masked_with(&broad);
        let thin_stack = masked_with(&thin);

        let plan = source
            .proxy_plan(&registry, &unmasked, bounds)
            .expect("a plan")
            .expect("a photo-sized source needs a proxy at this size");
        let proxy = source.proxy(plan).expect("a proxy");
        let stage = Stage {
            width: plan.width,
            height: plan.height,
        };
        assert!(
            registry
                .proxy_approximation(&broad_stack, stage.width, stage.height)
                .mask
                .eq(&false),
            "the broad mask must be resolvable at this proxy size"
        );
        assert!(
            registry
                .proxy_approximation(&thin_stack, stage.width, stage.height)
                .mask,
            "the thin mask must trip the supersample at this proxy size"
        );

        // `true` is the proxy phase's own entry point, which applies the thin-feature rule;
        // `false` is the same render with the mask point sampled, which is what the exact phase
        // does. Running one stack through both is the only comparison that isolates the rule's
        // cost: same bounds rectangle, same effect, four coverage evaluations against one.
        let measure = |name: &str, stack: &Recipe, thin_rule: bool| {
            let once = || {
                if thin_rule {
                    proxy.render_proxy_cancellable(
                        &registry,
                        SnapshotId::new(),
                        stack,
                        &Cancel::never(),
                    )
                } else {
                    proxy.render_cancellable(&registry, SnapshotId::new(), stack, &Cancel::never())
                }
                .expect("the stack renders at proxy size")
            };
            once();
            let mut samples = Vec::new();
            for _ in 0..25 {
                let start = std::time::Instant::now();
                let frame = once();
                samples.push(start.elapsed().as_secs_f64() * 1000.0);
                std::hint::black_box(frame);
            }
            let ms = luxforge_testbase::Distribution::of(samples).expect("renders ran");
            println!(
                "{fixture} proxy {}x{} {name}: p50 {:.2} ms, p95 {:.2} ms, min {:.2} ms, max {:.2} ms",
                plan.width, plan.height, ms.p50, ms.p95, ms.min, ms.max
            );
        };
        measure("unmasked full Basic", &unmasked, true);
        measure("broad mask, point sampled", &broad_stack, true);
        measure("thin mask, point sampled", &thin_stack, false);
        measure("thin mask, 2x2 supersampled", &thin_stack, true);
    }
}

/// A linear gradient down the frame whose ramp is `length` mask-space units, which is
/// `length x stage.height` pixels of whatever stage it is compiled against.
fn gradient_mask(length: f64) -> Mask {
    let mut mask = Mask::new("Mask 1");
    let name = mask.next_component_name("linear");
    mask.components.push(crate::Component::new(
        name,
        crate::ComponentMode::Add,
        "linear",
        json!({"x0": 0.5, "y0": 0.5 - length / 2.0, "x1": 0.5, "y1": 0.5 + length / 2.0}),
    ));
    mask
}

#[test]
fn a_plan_that_would_upscale_or_vanish_is_refused() {
    let source = jpeg_source(8, 6, &[[1, 2, 3]; 48]);
    assert!(source.proxy(plan(0, 3, (4, 3))).is_err());
    assert!(source.proxy(plan(4, 0, (4, 3))).is_err());
    assert!(source.proxy(plan(9, 3, (9, 3))).is_err());
    assert!(source.proxy(plan(4, 7, (4, 7))).is_err());
    assert!(source.proxy(plan(8, 6, (8, 6))).is_ok(), "1:1 is allowed");
    // A window must lie inside the proxy stage and hold a pixel.
    let windowed = |x, y, width, height| ProxyPlan {
        window: Some(ProxyWindow {
            x,
            y,
            width,
            height,
        }),
        ..plan(4, 3, (4, 3))
    };
    assert!(source.proxy(windowed(1, 1, 3, 2)).is_ok());
    assert!(source.proxy(windowed(2, 1, 3, 2)).is_err());
    assert!(source.proxy(windowed(0, 2, 4, 2)).is_err());
    assert!(source.proxy(windowed(0, 0, 0, 2)).is_err());
}

/// A windowed proxy source is the whole downscale's pixels in its window, bit for bit, at
/// fractional scales and for both source kinds: the same weights over the same source samples,
/// reading only the source rows and columns the window covers.
#[test]
fn a_windowed_proxy_is_the_whole_downscale_in_its_window() {
    let (width, height) = (53u32, 41u32);
    let codes: Vec<[u8; 3]> = (0..width * height)
        .map(|index| {
            [
                (index * 37 % 251) as u8,
                (index * 11 % 239) as u8,
                (index * 5 % 241) as u8,
            ]
        })
        .collect();
    let floats: Vec<[f32; 3]> = codes
        .iter()
        .map(|code| code.map(|value| f32::from(value) / 97.0 - 0.3))
        .collect();
    let sources = [
        jpeg_source(width, height, &codes),
        PreviewSource::Raw {
            image: raw_source(width, height, &floats)
                .with_view([2, 1, 49, 38], 6)
                .expect("a view"),
            settings: LinearSettings::default(),
        },
    ];
    for source in &sources {
        let (source_width, source_height) = source.dimensions();
        let whole_plan = plan(source_width * 3 / 7, source_height * 5 / 9, (1, 1));
        let whole = source.proxy(whole_plan).expect("the whole downscale");
        for (x, y, w, h) in [
            (0, 0, 1, 1),
            (3, 2, 7, 5),
            (whole_plan.width - 4, whole_plan.height - 3, 4, 3),
            (0, 0, whole_plan.width, whole_plan.height),
        ] {
            let windowed_plan = ProxyPlan {
                window: Some(ProxyWindow {
                    x,
                    y,
                    width: w,
                    height: h,
                }),
                ..whole_plan
            };
            let windowed = source.proxy(windowed_plan).expect("a windowed proxy");
            assert_eq!(windowed.dimensions(), (w, h));
            for row in 0..h {
                for column in 0..w {
                    match (&windowed, &whole) {
                        (PreviewSource::Jpeg(part), PreviewSource::Jpeg(all)) => {
                            let at = ((row * w + column) * 4) as usize;
                            let from = (((y + row) * whole_plan.width + x + column) * 4) as usize;
                            assert_eq!(
                                part.rgba[at..at + 4],
                                all.rgba[from..from + 4],
                                "({column}, {row}) of {w}x{h} at ({x}, {y})"
                            );
                        }
                        (
                            PreviewSource::Raw { image: part, .. },
                            PreviewSource::Raw { image: all, .. },
                        ) => {
                            let actual = part.pixel(column, row).unwrap();
                            let expected = all.pixel(x + column, y + row).unwrap();
                            assert!(
                                actual.map(f32::to_bits) == expected.map(f32::to_bits),
                                "({column}, {row}) of {w}x{h} at ({x}, {y})"
                            );
                        }
                        _ => unreachable!("a proxy keeps its source kind"),
                    }
                }
            }
        }
    }
}

/// The band size never changes a proxy: bands of one output row, which split every straddled
/// source row between two bands, and bands of a few rows build the same bytes as one band over
/// the whole window, which is one intermediate of every source row the window reads. For both
/// source kinds, at fractional scales, whole and windowed, serially and forced onto the
/// parallel path.
#[test]
fn a_proxy_is_the_same_bytes_at_any_band_size() {
    let pixels = |width: u32, height: u32| -> Vec<[u8; 3]> {
        (0..width * height)
            .map(|index| {
                [
                    (index * 37 % 251) as u8,
                    (index * 11 % 239) as u8,
                    (index * 5 % 241) as u8,
                ]
            })
            .collect()
    };
    let (width, height) = (53u32, 41u32);
    let floats: Vec<[f32; 3]> = pixels(width, height)
        .iter()
        .map(|code| code.map(|value| f32::from(value) / 97.0 - 0.3))
        .collect();
    let sources = [
        jpeg_source(width, height, &pixels(width, height)),
        PreviewSource::Raw {
            image: raw_source(width, height, &floats)
                .with_view([2, 1, 49, 38], 6)
                .expect("a view"),
            settings: LinearSettings::default(),
        },
    ];
    let bits = |source: &PreviewSource| -> Vec<u32> {
        match source {
            PreviewSource::Jpeg(image) => image.rgba.iter().map(|&code| code.into()).collect(),
            PreviewSource::Raw { image, .. } => {
                image.planes().iter().map(|value| value.to_bits()).collect()
            }
        }
    };
    for (pooled, source) in [false, true]
        .into_iter()
        .flat_map(|pooled| sources.iter().map(move |source| (pooled, source)))
    {
        crate::render::parallel::force(Some(pooled));
        let (source_width, source_height) = source.dimensions();
        let whole = plan(source_width * 3 / 7, source_height * 5 / 9, (1, 1));
        let windows = [
            None,
            Some(ProxyWindow {
                x: 3,
                y: 2,
                width: 7,
                height: whole.height - 4,
            }),
        ];
        for window in windows {
            let plan = ProxyPlan { window, ..whole };
            let stride = plan.source_dimensions().0 as usize * 3 * std::mem::size_of::<f32>();
            let one_band = source
                .proxy_in_bands(plan, &Cancel::never(), usize::MAX)
                .expect("one band");
            assert_eq!(
                BoxDownscale::new(source_width, source_height, plan, usize::MAX)
                    .expect("a downscale")
                    .bands(),
                1
            );
            for band_bytes in [1, stride * 3, stride * 5, stride * 11] {
                let downscale = BoxDownscale::new(source_width, source_height, plan, band_bytes)
                    .expect("a downscale");
                assert!(
                    downscale.bands() > 1,
                    "{band_bytes} bytes splits the window"
                );
                assert_eq!(downscale.parallel, pooled);
                let banded = source
                    .proxy_in_bands(plan, &Cancel::never(), band_bytes)
                    .expect("a banded proxy");
                assert_eq!(banded.dimensions(), one_band.dimensions());
                assert!(
                    bits(&banded) == bits(&one_band),
                    "{source_width}x{source_height} to {plan:?} in bands of {band_bytes} \
                     bytes, pooled {pooled}"
                );
            }
        }
    }
    crate::render::parallel::force(None);
}
